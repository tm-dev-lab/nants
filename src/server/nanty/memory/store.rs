//! Os arquivos da memória (spec 140 T07, C-21): `memory/<tema>.json` na pasta do Nanty, um por tema,
//! com `version` e `entries`.
//!
//! - **Falha fechada:** o arquivo inválido (JSON quebrado, sem versão, ilegível ou grande demais) é
//!   preservado como está, e o tema recusa gravação ([`MemoryError::Invalid`]) até alguém olhar; a
//!   leitura o trata como vazio. Na memória, recomeçar do vazio apagaria tudo na gravação seguinte.
//! - **Versão mais nova:** só leitura ([`MemoryError::Newer`]); o que der para ler, aparece.
//! - **Gravação:** sob uma trava do processo que vale para ler e mudar; antes de regravar, a versão
//!   atual vai para `<tema>.json.bak`; as duas gravações são atômicas (`write_atomic`: temporário 0600
//!   e `rename`).
//! - **Só o dono da instância grava:** a conferência é do [`super::super::Nanty`]
//!   (`memory_write`); a outra instância recebe erro com código.
//! - **A pasta nunca é montada em container:** os containers dos projetos só montam a worktree (ver o
//!   teste em `harness::compose`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use super::{Entry, EntryState, MemoryFile, Topic, MEMORY_VERSION};
use crate::server::nanty::jsonl::{read_private, write_atomic};

/// Maior arquivo de tema lido (1 MiB).
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Como o arquivo de um tema foi achado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// Ainda não existe: nasce na primeira gravação.
    Missing,
    Ok,
    /// Gravado por uma versão mais nova: só leitura.
    Newer,
    /// Não deu para ler: preservado, sem gravação.
    Invalid,
}

impl FileStatus {
    pub fn writable(self) -> bool {
        matches!(self, FileStatus::Missing | FileStatus::Ok)
    }
}

/// Um tema lido.
#[derive(Debug, Clone, PartialEq)]
pub struct TopicFile {
    pub topic: Topic,
    pub status: FileStatus,
    pub entries: Vec<Entry>,
}

/// Todos os temas lidos.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub topics: Vec<TopicFile>,
}

impl Snapshot {
    /// Todas as entradas, ativas ou não.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.topics.iter().flat_map(|t| t.entries.iter())
    }

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.entries().find(|e| e.id == id)
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.topics.iter_mut().flat_map(|t| t.entries.iter_mut()).find(|e| e.id == id)
    }

    /// As entradas ativas, somando os temas.
    pub fn active(&self) -> usize {
        self.entries().filter(|e| e.active()).count()
    }

    /// O tema, criado vazio se faltar (um `Snapshot` vazio, sem pasta).
    pub fn topic_mut(&mut self, topic: Topic) -> &mut TopicFile {
        if let Some(i) = self.topics.iter().position(|t| t.topic == topic) {
            return &mut self.topics[i];
        }
        self.topics.push(TopicFile { topic, status: FileStatus::Missing, entries: Vec::new() });
        let last = self.topics.len() - 1;
        &mut self.topics[last]
    }

    pub fn status(&self, topic: Topic) -> FileStatus {
        self.topics.iter().find(|t| t.topic == topic).map_or(FileStatus::Missing, |t| t.status)
    }

    /// Tira a entrada de um tema e a põe em outro (a edição que troca o tema).
    pub fn move_entry(&mut self, id: &str, to: Topic) -> bool {
        let Some(from) = self.topics.iter().position(|t| t.entries.iter().any(|e| e.id == id)) else { return false };
        if self.topics[from].topic == to {
            return true;
        }
        let Some(at) = self.topics[from].entries.iter().position(|e| e.id == id) else { return false };
        let mut entry = self.topics[from].entries.remove(at);
        entry.topic = to;
        self.topic_mut(to).entries.push(entry);
        true
    }

    /// Mantém só as entradas que passam em `keep`; `true` se tirou alguma.
    pub fn retain(&mut self, keep: impl Fn(&Entry) -> bool) -> bool {
        let mut changed = false;
        for topic in &mut self.topics {
            let before = topic.entries.len();
            topic.entries.retain(|e| keep(e));
            changed |= before != topic.entries.len();
        }
        changed
    }

    /// As apagadas há mais que `keep_ms` saem do arquivo.
    pub fn purge_deleted(&mut self, now_ms: u64, keep_ms: u64) -> bool {
        self.retain(|e| e.state != EntryState::Deleted || e.deleted_at.is_none_or(|at| now_ms.saturating_sub(at) <= keep_ms))
    }
}

/// Falha da memória, com código para o registro e para a rota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    /// O tema foi gravado por uma versão mais nova: só leitura.
    Newer(Topic),
    /// O arquivo do tema não dá para ler: preservado, sem gravação.
    Invalid(Topic),
    /// Não deu para gravar.
    Io,
    /// Outra instância é a dona (ou não há pasta de dados).
    NotOwner,
}

impl MemoryError {
    pub fn code(self) -> &'static str {
        match self {
            MemoryError::Newer(_) => "memory_newer",
            MemoryError::Invalid(_) => "memory_invalid",
            MemoryError::Io => "memory_io",
            MemoryError::NotOwner => "memory_not_owner",
        }
    }

    pub fn topic(self) -> Option<Topic> {
        match self {
            MemoryError::Newer(t) | MemoryError::Invalid(t) => Some(t),
            MemoryError::Io | MemoryError::NotOwner => None,
        }
    }
}

/// A pasta da memória, com a trava de ler e mudar.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    dir: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl MemoryStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, lock: Arc::new(Mutex::new(())) }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn path(&self, topic: Topic) -> PathBuf {
        self.dir.join(format!("{}.json", topic.as_str()))
    }

    pub fn backup(&self, topic: Topic) -> PathBuf {
        self.dir.join(format!("{}.json.bak", topic.as_str()))
    }

    fn read(&self, topic: Topic) -> TopicFile {
        let path = self.path(topic);
        let file = |status, entries| TopicFile { topic, status, entries };
        if std::fs::symlink_metadata(&path).is_err() {
            return file(FileStatus::Missing, Vec::new());
        }
        let Some(text) = read_private(&path, MAX_FILE_BYTES) else { return file(FileStatus::Invalid, Vec::new()) };
        let Ok(raw) = serde_json::from_str::<Value>(&text) else { return file(FileStatus::Invalid, Vec::new()) };
        let version = raw.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version == 0 {
            return file(FileStatus::Invalid, Vec::new());
        }
        let own = |mut entries: Vec<Entry>| {
            entries.iter_mut().for_each(|e| e.topic = topic);
            entries
        };
        if version > u64::from(MEMORY_VERSION) {
            // O que der para ler da versão nova; o resto fica no arquivo, intocado.
            let entries = raw
                .get("entries")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(|v| serde_json::from_value::<Entry>(v.clone()).ok()).collect())
                .unwrap_or_default();
            return file(FileStatus::Newer, own(entries));
        }
        match serde_json::from_value::<MemoryFile>(raw) {
            Ok(parsed) => file(FileStatus::Ok, own(parsed.entries)),
            Err(_) => file(FileStatus::Invalid, Vec::new()),
        }
    }

    fn read_all(&self) -> Snapshot {
        Snapshot { topics: Topic::ALL.iter().map(|t| self.read(*t)).collect() }
    }

    /// Lê todos os temas.
    pub fn load(&self) -> Snapshot {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.read_all()
    }

    /// Muda a memória com `f` e grava os temas que mudaram. Um tema mudado que não pode ser gravado
    /// (inválido ou de versão mais nova) recusa tudo, sem gravar nada. Devolve o que `f` devolveu e se
    /// algo foi gravado.
    pub fn transact<T>(&self, f: impl FnOnce(&mut Snapshot) -> T) -> Result<(T, bool), MemoryError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.read_all();
        let mut after = before.clone();
        let out = f(&mut after);
        let changed: Vec<&TopicFile> = after
            .topics
            .iter()
            .filter(|t| before.topics.iter().find(|b| b.topic == t.topic).is_none_or(|b| b.entries != t.entries))
            .collect();
        for topic in &changed {
            match before.status(topic.topic) {
                FileStatus::Newer => return Err(MemoryError::Newer(topic.topic)),
                FileStatus::Invalid => return Err(MemoryError::Invalid(topic.topic)),
                FileStatus::Missing | FileStatus::Ok => {}
            }
        }
        for topic in &changed {
            self.write(topic.topic, &topic.entries)?;
        }
        Ok((out, !changed.is_empty()))
    }

    /// Grava um tema: a versão atual vai antes para o `.bak`.
    fn write(&self, topic: Topic, entries: &[Entry]) -> Result<(), MemoryError> {
        let path = self.path(topic);
        if let Some(current) = read_private(&path, MAX_FILE_BYTES) {
            write_atomic(&self.backup(topic), current.as_bytes()).map_err(|_| MemoryError::Io)?;
        }
        let file = MemoryFile { version: MEMORY_VERSION, entries: entries.to_vec() };
        let body = serde_json::to_vec_pretty(&file).map_err(|_| MemoryError::Io)?;
        write_atomic(&path, &body).map_err(|_| MemoryError::Io)
    }

    /// Mantém, tema a tema, só as entradas que passam em `keep`. O tema que perderia entradas e não pode
    /// ser gravado fica intocado; os outros gravam mesmo assim. Devolve as recusas, uma por tema.
    pub fn retain_writable(&self, keep: impl Fn(&Entry) -> bool) -> Vec<MemoryError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut errors = Vec::new();
        for topic in self.read_all().topics {
            let kept: Vec<Entry> = topic.entries.iter().filter(|e| keep(e)).cloned().collect();
            if kept.len() == topic.entries.len() {
                continue;
            }
            let result = match topic.status {
                FileStatus::Newer => Err(MemoryError::Newer(topic.topic)),
                FileStatus::Invalid => Err(MemoryError::Invalid(topic.topic)),
                FileStatus::Missing | FileStatus::Ok => self.write(topic.topic, &kept),
            };
            if let Err(e) = result {
                errors.push(e);
            }
        }
        errors
    }

    /// Regrava os `.bak` só com as entradas que passam em `keep` (a limpeza de projeto: a cópia
    /// anterior não guarda o que saiu). O `.bak` que não dá para ler fica como está.
    pub fn scrub_backups(&self, keep: impl Fn(&Entry) -> bool) {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        for topic in Topic::ALL {
            let path = self.backup(topic);
            let Some(text) = read_private(&path, MAX_FILE_BYTES) else { continue };
            let Ok(mut file) = serde_json::from_str::<MemoryFile>(&text) else { continue };
            if file.version > MEMORY_VERSION {
                continue;
            }
            let before = file.entries.len();
            file.entries.retain(|e| keep(e));
            if file.entries.len() != before {
                if let Ok(body) = serde_json::to_vec_pretty(&file) {
                    let _ = write_atomic(&path, &body);
                }
            }
        }
    }
}
