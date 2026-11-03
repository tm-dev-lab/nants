//! As mudanças da memória (spec 140 T07): gravar, trocar, apagar, devolver, editar e marcar o uso.
//! Todas passam pelo redator único antes de gravar ([`crate::server::nanty::redact::scrub`], C-09) e
//! pela gravação do dono ([`crate::server::nanty::Nanty::memory_write`]).

use super::store::MemoryError;
use super::{tidy, Entry, EntryState, Source, Topic, TITLE_MAX_CHARS};
use crate::config::{Config, NantyMemory};
use crate::server::events::SecretSet;
use crate::server::nanty::{redact, Nanty};

/// Por que uma mudança não foi feita.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    TitleEmpty,
    TitleLong,
    TextEmpty,
    /// Passou do teto do tema (o teto vai junto).
    TextLong(usize),
    /// Já há o máximo de entradas ativas.
    Full(usize),
    NotFound,
    /// Já existe uma entrada ativa com o mesmo título no tema (o id dela).
    Duplicate(String),
    /// A edição partiu de outra versão (a atual vai junto).
    Conflict(Box<Entry>),
    /// A entrada apagada já não pode voltar.
    Gone,
    /// A nota que o pedido troca mudou depois da proposta: nada é sobrescrito.
    Changed,
    File(MemoryError),
}

/// O pedido de gravar ou trocar uma entrada.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Remember {
    pub topic: Topic,
    pub title: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Na troca, o `updated` da nota que o pedido viu: se ela mudou, a troca é recusada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_updated: Option<u64>,
}

/// Título e texto como ficam gravados: redigidos (antes de qualquer corte), sem controle e dentro dos
/// tetos. Passou do teto é recusa, nunca corte: o modelo ou a pessoa reescreve mais curto.
pub fn clean(cfg: &NantyMemory, secrets: &SecretSet, topic: Topic, title: &str, text: &str) -> Result<(String, String), Refusal> {
    let title = tidy(&redact::scrub(secrets, title)).replace('\n', " ");
    let text = tidy(&redact::scrub(secrets, text));
    if title.is_empty() {
        return Err(Refusal::TitleEmpty);
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(Refusal::TitleLong);
    }
    if text.is_empty() {
        return Err(Refusal::TextEmpty);
    }
    let max = topic.max_chars(cfg);
    if text.chars().count() > max {
        return Err(Refusal::TextLong(max));
    }
    Ok((title, text))
}

/// A versão seguinte: sempre maior que a anterior, mesmo com o relógio parado.
fn bump(previous: u64, now: u64) -> u64 {
    now.max(previous.saturating_add(1))
}

fn flatten<T>(result: Result<Result<T, Refusal>, MemoryError>) -> Result<T, Refusal> {
    result.map_err(Refusal::File).and_then(|inner| inner)
}

/// Grava (ou troca, com `replace_id`) a entrada, com a procedência dada. O texto já passou por
/// [`clean`].
pub fn save(nanty: &Nanty, cfg: &Config, remember: &Remember, source: Source) -> Result<Entry, Refusal> {
    let memory = &cfg.nanty.memory;
    let now = source.at_ms;
    flatten(nanty.memory_write(memory, |snap| {
        if let Some(id) = &remember.replace_id {
            let Some(old) = snap.find(id).filter(|e| e.active()).cloned() else { return Err(Refusal::NotFound) };
            if remember.base_updated.is_some_and(|base| base != old.updated) {
                return Err(Refusal::Changed);
            }
            // Revisão 1 da T07 (F-01): a versão anterior vai para as apagadas, e o `restore` a traz de
            // volta no mesmo prazo de qualquer exclusão.
            let previous = Entry {
                id: crate::server::nanty::new_id(),
                state: EntryState::Deleted,
                deleted_at: Some(now),
                updated: bump(old.updated, now),
                ..old
            };
            snap.topic_mut(previous.topic).entries.push(previous);
            snap.move_entry(id, remember.topic);
            let Some(entry) = snap.find_mut(id) else { return Err(Refusal::NotFound) };
            entry.title.clone_from(&remember.title);
            entry.text.clone_from(&remember.text);
            entry.source = source;
            entry.updated = bump(entry.updated, now);
            entry.last_used = now;
            return Ok(entry.clone());
        }
        let same = |e: &&Entry| e.active() && e.topic == remember.topic && e.title.to_lowercase() == remember.title.to_lowercase();
        if let Some(existing) = snap.entries().find(same) {
            return Err(Refusal::Duplicate(existing.id.clone()));
        }
        if snap.active() >= memory.active() {
            return Err(Refusal::Full(memory.active()));
        }
        let entry = Entry {
            id: crate::server::nanty::new_id(),
            topic: remember.topic,
            title: remember.title.clone(),
            text: remember.text.clone(),
            source,
            created: now,
            updated: now,
            last_used: now,
            state: EntryState::Active,
            deleted_at: None,
        };
        snap.topic_mut(remember.topic).entries.push(entry.clone());
        Ok(entry)
    }))
}

/// Apaga a entrada ativa: ela ainda volta por um tempo (`deleted_keep_days`).
pub fn delete(nanty: &Nanty, cfg: &Config, id: &str) -> Result<Entry, Refusal> {
    let now = nanty.now().epoch_ms;
    flatten(nanty.memory_write(&cfg.nanty.memory, |snap| {
        let Some(entry) = snap.find_mut(id).filter(|e| e.active()) else { return Err(Refusal::NotFound) };
        entry.state = EntryState::Deleted;
        entry.deleted_at = Some(now);
        entry.updated = bump(entry.updated, now);
        Ok(entry.clone())
    }))
}

/// Devolve a entrada apagada, se ainda está no arquivo e cabe entre as ativas.
///
/// Spec 140 T17 (da T07): se uma nota ativa do mesmo tema já tem o mesmo título (a versão que
/// substituiu esta, num `replace_id`), ela troca de lugar com a devolvida: vai para as apagadas, com o
/// mesmo prazo de volta, e o tema nunca fica com duas notas ativas de mesmo título. Na troca, o número
/// de ativas não muda, então o teto não recusa. Devolve a nota devolvida e as que foram para as
/// apagadas na troca, para o registro (`memory.swapped`, em [`super::api::post_restore`]).
pub fn restore(nanty: &Nanty, cfg: &Config, id: &str) -> Result<(Entry, Vec<Entry>), Refusal> {
    let now = nanty.now().epoch_ms;
    let max = cfg.nanty.memory.active();
    flatten(nanty.memory_write(&cfg.nanty.memory, |snap| {
        let Some(back) = snap.find(id).cloned() else { return Err(Refusal::Gone) };
        if back.state != EntryState::Deleted {
            return Err(Refusal::NotFound);
        }
        let title = back.title.to_lowercase();
        let same: Vec<String> =
            snap.entries().filter(|e| e.active() && e.id != back.id && e.topic == back.topic && e.title.to_lowercase() == title).map(|e| e.id.clone()).collect();
        if same.is_empty() && snap.active() >= max {
            return Err(Refusal::Full(max));
        }
        let mut swapped = Vec::new();
        for other in &same {
            if let Some(e) = snap.find_mut(other) {
                e.state = EntryState::Deleted;
                e.deleted_at = Some(now);
                e.updated = bump(e.updated, now);
                swapped.push(e.clone());
            }
        }
        let Some(entry) = snap.find_mut(id) else { return Err(Refusal::Gone) };
        entry.state = EntryState::Active;
        entry.deleted_at = None;
        entry.updated = bump(entry.updated, now);
        Ok((entry.clone(), swapped))
    }))
}

/// A edição da pessoa pela aba: vale só sobre a versão que ela leu (`base_updated`) e grava como
/// `person_said`, com o projeto de antes.
pub fn edit(nanty: &Nanty, cfg: &Config, id: &str, title: &str, text: &str, base_updated: u64, source: Source) -> Result<Entry, Refusal> {
    let now = source.at_ms;
    flatten(nanty.memory_write(&cfg.nanty.memory, |snap| {
        let Some(entry) = snap.find_mut(id).filter(|e| e.active()) else { return Err(Refusal::NotFound) };
        if entry.updated != base_updated {
            return Err(Refusal::Conflict(Box::new(entry.clone())));
        }
        let project = entry.source.project_id.clone();
        entry.title = title.to_string();
        entry.text = text.to_string();
        entry.source = Source { project_id: project, ..source };
        entry.updated = bump(entry.updated, now);
        Ok(entry.clone())
    }))
}

/// Marca o uso (o `recall` trouxe a entrada): ela volta ao índice. É metadado, não muda o texto.
pub fn touch(nanty: &Nanty, cfg: &Config, ids: &[String]) {
    let now = nanty.now().epoch_ms;
    let _ = nanty.memory_write(&cfg.nanty.memory, |snap| {
        for id in ids {
            if let Some(entry) = snap.find_mut(id) {
                entry.last_used = now;
            }
        }
    });
}
