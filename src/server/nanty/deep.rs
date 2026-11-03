//! Leitura profunda a pedido, na conversa do Nanty (spec 140 T10, G5).
//!
//! Quatro ferramentas, uma por família, só de leitura e só na conversa ([`super::tools::Mode`]):
//! - **`agent_log`:** o diálogo dos agentes (`.sdd/specs/<spec>/dialog.jsonl`, de
//!   [`crate::server::dialog`]) ou a trilha de ferramentas (`tools.jsonl`, de
//!   [`crate::server::tools_trail`]), lidos de trás para a frente: o fim é o que costuma importar.
//!   Filtros por tarefa e por trecho de texto, e `skip` para voltar mais.
//! - **`sdd_docs`:** as specs, a spec, as tarefas, uma tarefa, a evidência dos testes (o
//!   `test_evidence` da tarefa, do fim para trás) e as análises de recuperação e de ambiente (as seções
//!   que o runner grava no corpo da tarefa).
//! - **`project_files`:** listagem e leitura por faixa de qualquer arquivo do projeto, inclusive o
//!   `ARCHITECTURE.md` e o roadmap (C-26).
//! - **`git_history`:** os últimos commits, pelo Git neutro da T09 ([`super::git_log`]).
//!
//! **Tudo pelo leitor da T09** ([`super::files::Root`]): sem link, sem pasta bloqueada, sem nome de
//! segredo, sem chave privada nos arquivos do projeto e sem binário. Os registros JSONL dos agentes são
//! lidos pela cauda ([`super::files::Root::tail`]), até [`LOG_MAX_BYTES`]; cada campo passa inteiro pelo
//! redator ([`redact::scrub`], que cobre PEM) antes de qualquer corte.
//!
//! **Tetos:** o do turno ([`Budget`], `[nanty.read]`) vale para a soma de tudo o que volta ao modelo;
//! cada item tem o seu teto de caracteres; e o número de itens por chamada é limitado.
//!
//! **Envelope:** o resultado vai no envelope de dado não confiável ([`files::envelope`]), com os nomes
//! dentro. Os erros vão como código (`nanty_read_*`, `nanty_git_*`), com a frase do dicionário.
//!
//! **Procedência e isolamento** ficam com o despachante ([`super::tools::call`]): toda leitura
//! contamina o turno com o tipo e o projeto, e um segundo projeto só entra no mesmo pedido do modelo que
//! o primeiro ([`super::agent::Session::admit`], C-16).

use serde_json::{json, Value};

use super::files::{self, Budget, LineRange, ReadError, Root};
use super::provenance::TaintKind;
use super::turn::{is_code, is_task_id};
use super::{git_log, history, redact};
use crate::config::{Config, NantyRead, Project};
use crate::i18n::{tr, Lang};
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Diálogo e trilha dos agentes.
pub const AGENT_LOG: &str = "agent_log";
/// Specs, tarefas, evidência dos testes e análises de recuperação.
pub const SDD_DOCS: &str = "sdd_docs";
/// Arquivos do projeto.
pub const PROJECT_FILES: &str = "project_files";
/// Histórico do Git.
pub const GIT_HISTORY: &str = "git_history";
/// As quatro, na ordem em que vão ao modelo.
pub const TOOLS: [&str; 4] = [AGENT_LOG, SDD_DOCS, PROJECT_FILES, GIT_HISTORY];

/// Pasta das specs.
const SPECS: &str = ".sdd/specs";
/// O maior trecho lido do fim de um registro JSONL (o diálogo e a trilha têm teto de 2 MiB na gravação).
pub const LOG_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Itens do registro por chamada: o padrão e o máximo.
const LOG_ITEMS: usize = 20;
const LOG_MAX_ITEMS: usize = 50;
/// Teto do texto de uma mensagem do diálogo e de uma ação da trilha, em caracteres.
const DIALOG_TEXT_CHARS: usize = 1500;
const TRAIL_TEXT_CHARS: usize = 600;
/// Teto do filtro de texto, em caracteres.
const FILTER_MAX_CHARS: usize = 100;
/// Linhas da evidência dos testes, do fim para trás, quando o modelo não diz.
const TESTS_LINES: usize = 120;
/// Tarefas e specs listadas, no máximo.
const MAX_TASKS: usize = 80;
/// Commits por chamada, quando o modelo não diz.
const GIT_COMMITS: usize = 15;

/// Uma família, com os parâmetros já conferidos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Log { source: LogSource, spec: Option<String>, task: Option<String>, contains: Option<String>, skip: usize, limit: usize },
    Docs { part: Part, spec: Option<String>, task: Option<String>, start: Option<usize>, lines: Option<usize> },
    Files { path: String, start: Option<usize>, lines: Option<usize> },
    Git { count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogSource {
    Dialog,
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Specs,
    Spec,
    Tasks,
    Task,
    Tests,
    Recovery,
}

impl Request {
    /// Os parâmetros de uma chamada. `None`: algum valor fora da forma esperada.
    pub fn parse(name: &str, args: &Value) -> Option<Self> {
        let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty());
        let number = |key: &str| args.get(key).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok());
        // Valor presente com o tipo errado é recusado, nunca ignorado.
        let typed = |key: &str, ok: fn(&Value) -> bool| args.get(key).is_none_or(|v| v.is_null() || ok(v));
        let all_typed = ["spec_id", "task_id", "contains", "path", "source", "part"].iter().all(|k| typed(k, Value::is_string))
            && ["skip", "limit", "start_line", "lines", "count"].iter().all(|k| typed(k, Value::is_u64));
        if !all_typed {
            return None;
        }
        let spec = match text("spec_id") {
            Some(s) if is_spec_id(s) => Some(s.to_string()),
            Some(_) => return None,
            None => None,
        };
        let task = match text("task_id") {
            Some(t) if is_task_id(t) || t == "FINAL" => Some(t.to_string()),
            Some(_) => return None,
            None => None,
        };
        let start = number("start_line").filter(|n| *n > 0);
        let lines = number("lines").filter(|n| *n > 0);
        match name {
            AGENT_LOG => {
                let source = match text("source")? {
                    "dialog" => LogSource::Dialog,
                    "tools" => LogSource::Tools,
                    _ => return None,
                };
                let contains = text("contains").map(|c| c.chars().take(FILTER_MAX_CHARS).collect::<String>().to_lowercase());
                let skip = number("skip").unwrap_or(0);
                let limit = number("limit").unwrap_or(LOG_ITEMS).clamp(1, LOG_MAX_ITEMS);
                Some(Self::Log { source, spec, task, contains, skip, limit })
            }
            SDD_DOCS => {
                let part = match text("part")? {
                    "specs" => Part::Specs,
                    "spec" => Part::Spec,
                    "tasks" => Part::Tasks,
                    "task" => Part::Task,
                    "tests" => Part::Tests,
                    "recovery" => Part::Recovery,
                    _ => return None,
                };
                Some(Self::Docs { part, spec, task, start, lines })
            }
            PROJECT_FILES => Some(Self::Files { path: text("path").unwrap_or_default().to_string(), start, lines }),
            GIT_HISTORY => Some(Self::Git { count: number("count").unwrap_or(GIT_COMMITS).max(1) }),
            _ => None,
        }
    }

    /// O tipo de leitura gravado na procedência (C-05).
    pub fn taint(&self) -> TaintKind {
        match self {
            Self::Log { source: LogSource::Dialog, .. } => TaintKind::AgentDialog,
            Self::Log { source: LogSource::Tools, .. } => TaintKind::ToolTrail,
            // Spec 140 T17 (da T10): a tarefa inteira traz a evidência dos testes no corpo.
            Self::Docs { part: Part::Tests | Part::Task, .. } => TaintKind::TestOutput,
            Self::Docs { .. } | Self::Files { .. } => TaintKind::ProjectFile,
            Self::Git { .. } => TaintKind::GitHistory,
        }
    }

    fn needs_task(&self) -> bool {
        matches!(self, Self::Docs { part: Part::Task | Part::Tests | Part::Recovery, .. })
    }

    fn needs_spec(&self) -> bool {
        matches!(self, Self::Log { .. } | Self::Docs { part: Part::Spec | Part::Tasks | Part::Task | Part::Tests | Part::Recovery, .. })
    }
}

/// O nome de uma pasta de spec com a forma do SDD (o mesmo filtro do relatório em [`super::read`]).
fn is_spec_id(name: &str) -> bool {
    is_code(name) && crate::server::spec_result::validate_spec_id(name).is_ok()
}

/// Por que não houve leitura: o código do leitor ou do Git, ou a falta da tarefa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Read(ReadError),
    Git(git_log::GitReadError),
    /// Nenhuma spec no projeto.
    NoSpec,
    /// A fase atual não aponta tarefa, e o modelo não disse qual.
    NoTask,
    /// O arquivo existe, mas não tem a forma de uma tarefa do SDD.
    Invalid,
}

impl From<ReadError> for Failure {
    fn from(e: ReadError) -> Self {
        Self::Read(e)
    }
}

impl Failure {
    fn code(self) -> &'static str {
        match self {
            Self::Read(e) => e.code(),
            Self::Git(e) => e.code(),
            Self::NoSpec => "nanty_read_no_spec",
            Self::NoTask => "nanty_read_no_task",
            Self::Invalid => "nanty_read_invalid",
        }
    }
}

/// O texto de um código de erro da leitura no idioma: a frase do dicionário, com o código.
pub fn failure_text(lang: Lang, code: &str) -> String {
    let why = tr(lang, &format!("nanty.read_error.{code}"), &[]);
    tr(lang, "nanty.tool.read_failed", &[("code", code), ("why", &why)])
}

/// Executa uma leitura de um projeto já conferido (observado e admitido pelo despachante) e devolve o
/// texto para o modelo: a introdução e o envelope, ou o erro com o código.
pub async fn read(state: &AppState, cfg: &Config, lang: Lang, secrets: &SecretSet, project: &Project, request: Request, budget: &mut Budget) -> String {
    let limits = cfg.nanty.read.clone();
    // A spec e a tarefa da fase, pela fonte única, quando o modelo não disse (fatos do servidor).
    let mut request = request;
    if request.needs_spec() || request.needs_task() {
        let facts = super::observe::project_facts(state, cfg, &project.id).await;
        let phase_spec = facts.as_ref().and_then(|f| f.phase_spec_id()).filter(|s| is_spec_id(s)).map(str::to_string);
        let phase_task = facts.as_ref().and_then(|f| f.phase_task_id()).filter(|t| is_task_id(t)).map(str::to_string);
        match &mut request {
            Request::Log { spec, .. } => {
                if spec.is_none() {
                    *spec = phase_spec;
                }
            }
            Request::Docs { spec, task, .. } => {
                // A tarefa da fase só vale na spec da fase.
                if spec.is_none() {
                    *spec = phase_spec;
                    if task.is_none() {
                        *task = phase_task;
                    }
                }
            }
            _ => {}
        }
    }
    let path = std::path::PathBuf::from(&project.path);
    let outcome = match request {
        Request::Git { count } => git(&path, &limits, secrets, count, budget).await,
        other => {
            let secrets = secrets.clone();
            let mut left = *budget;
            let job = tokio::task::spawn_blocking(move || {
                let out = Root::open(&path).map_err(Failure::from).and_then(|root| run(&root, &other, &limits, &secrets, &mut left));
                (out, left)
            });
            match job.await {
                Ok((out, left)) => {
                    *budget = left;
                    out
                }
                Err(_) => Err(Failure::Read(ReadError::Io)),
            }
        }
    };
    match outcome {
        Ok(value) => {
            let intro = tr(lang, "nanty.tool.deep_intro", &[("id", project.id.as_str())]);
            format!("{intro}\n{}", files::envelope(&value))
        }
        Err(failure) => failure_text(lang, failure.code()),
    }
}

/// O Git: assíncrono, pelo módulo da T09.
async fn git(path: &std::path::Path, limits: &NantyRead, secrets: &SecretSet, count: usize, budget: &mut Budget) -> Result<Value, Failure> {
    if budget.left() == 0 {
        return Err(Failure::Read(ReadError::Budget));
    }
    let owned = path.to_path_buf();
    let root = tokio::task::spawn_blocking(move || Root::open(&owned)).await.map_err(|_| Failure::Read(ReadError::Io))??;
    let commits = git_log::log(&root, limits, secrets, count).await.map_err(Failure::Git)?;
    let mut fit = Fit::new(budget);
    let mut items = Vec::new();
    for commit in &commits {
        let item = commit.to_json();
        if !fit.take(&item) {
            break;
        }
        items.push(item);
    }
    let truncated = items.len() < commits.len();
    fit.close(budget);
    if items.is_empty() && !commits.is_empty() {
        return Err(Failure::Read(ReadError::Budget));
    }
    Ok(json!({ "commits": items, "truncated": truncated }))
}

/// O que cabe no teto do turno, somado item a item (o tamanho do JSON de cada um).
struct Fit {
    left: usize,
    used: usize,
}

impl Fit {
    fn new(budget: &Budget) -> Self {
        Self { left: usize::try_from(budget.left()).unwrap_or(usize::MAX), used: 0 }
    }

    fn take(&mut self, item: &Value) -> bool {
        let n = serde_json::to_string(item).map(|s| s.len() + 1).unwrap_or(usize::MAX);
        if n > self.left {
            return false;
        }
        self.left -= n;
        self.used += n;
        true
    }

    fn close(self, budget: &mut Budget) {
        budget.spend(self.used);
    }
}

/// As leituras de disco (bloqueiam: o chamador usa `spawn_blocking`).
fn run(root: &Root, request: &Request, limits: &NantyRead, secrets: &SecretSet, budget: &mut Budget) -> Result<Value, Failure> {
    if budget.left() == 0 {
        return Err(Failure::Read(ReadError::Budget));
    }
    match request {
        Request::Log { source, spec, task, contains, skip, limit } => {
            let spec = spec_or_latest(root, spec.as_deref(), limits, secrets)?;
            log(root, &spec, *source, task.as_deref(), contains.as_deref(), *skip, *limit, limits, secrets, budget)
        }
        Request::Docs { part: Part::Specs, .. } => {
            let listing = root.list(SPECS, limits, secrets, budget)?;
            Ok(listing.to_json())
        }
        Request::Docs { part, spec, task, start, lines } => {
            let spec = spec_or_latest(root, spec.as_deref(), limits, secrets)?;
            docs(root, &spec, *part, task.as_deref(), *start, *lines, limits, secrets, budget)
        }
        Request::Files { path, start, lines } => files_read(root, path, *start, *lines, limits, secrets, budget),
        Request::Git { .. } => Err(Failure::Read(ReadError::Io)),
    }
}

/// A spec pedida ou, sem ela (e sem a da fase), a mais recente pelo nome, como o relatório.
fn spec_or_latest(root: &Root, spec: Option<&str>, limits: &NantyRead, secrets: &SecretSet) -> Result<String, Failure> {
    if let Some(spec) = spec {
        return Ok(spec.to_string());
    }
    // Os nomes não voltam ao modelo aqui: o teto do turno não paga esta listagem.
    let mut scratch = Budget::new(limits);
    let listing = root.list(SPECS, limits, secrets, &mut scratch).map_err(|e| if e == ReadError::Missing { Failure::NoSpec } else { Failure::Read(e) })?;
    let mut names: Vec<String> = listing.entries.into_iter().filter(|e| e.kind == files::EntryKind::Dir).map(|e| e.name).filter(|n| is_spec_id(n)).collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.into_iter().next().ok_or(Failure::NoSpec)
}

/// O texto redigido inteiro e, depois, cortado em `max` caracteres. `true` se cortou.
fn clean(secrets: &SecretSet, text: &str, max: usize) -> (String, bool) {
    let scrubbed = redact::scrub(secrets, text);
    let cut = scrubbed.chars().count() > max;
    (if cut { scrubbed.chars().take(max).collect() } else { scrubbed }, cut)
}

/// Um item do registro, já redigido.
struct LogItem {
    at: u64,
    agent: String,
    task: Option<String>,
    /// Só na trilha: o tipo da ação e a etapa.
    kind: Option<String>,
    stage: Option<String>,
    text: String,
}

impl LogItem {
    fn to_json(&self, max: usize) -> Value {
        let (text, cut) = (self.text.chars().take(max).collect::<String>(), self.text.chars().count() > max);
        let mut item = json!({ "at": history::stamp(self.at.saturating_mul(1000)), "agent": self.agent, "text": text });
        if cut {
            item["text_cut"] = json!(true);
        }
        if let Some(task) = &self.task {
            item["task"] = json!(task);
        }
        if let Some(kind) = &self.kind {
            item["kind"] = json!(kind);
        }
        if let Some(stage) = &self.stage {
            item["stage"] = json!(stage);
        }
        item
    }
}

/// As linhas completas da cauda: sem a primeira, se a cauda não começa no início do arquivo.
fn whole_lines(tail: &files::Tail) -> String {
    match (tail.from_start, tail.text.split_once('\n')) {
        (true, _) => tail.text.clone(),
        (false, Some((_, rest))) => rest.to_string(),
        (false, None) => String::new(),
    }
}

fn parse_log(source: LogSource, text: &str, secrets: &SecretSet) -> Vec<LogItem> {
    let one = |raw: &str| redact::scrub(secrets, raw);
    let task = |t: Option<&str>| t.filter(|t| is_task_id(t) || *t == "FINAL").map(str::to_string);
    match source {
        LogSource::Dialog => text
            .lines()
            .filter_map(|line| serde_json::from_str::<crate::server::dialog::DialogLine>(line).ok())
            .map(|l| LogItem { at: l.ts, agent: one(&l.agent), task: task(l.task_id.as_deref()), kind: None, stage: None, text: one(&l.text) })
            .collect(),
        LogSource::Tools => crate::server::tools_trail::parse_items(text.as_bytes())
            .into_iter()
            .map(|i| LogItem {
                at: i.ts,
                agent: one(&i.agent),
                task: task(i.task_id.as_deref()),
                kind: Some(one(&i.kind)),
                stage: i.stage.as_deref().map(one),
                text: one(&i.text),
            })
            .collect(),
    }
}

/// O fim do diálogo ou da trilha: a cauda cresce até ter itens bastantes, o arquivo todo ou
/// [`LOG_MAX_BYTES`]. Os itens vão do mais novo para trás até o teto, e voltam em ordem cronológica.
#[allow(clippy::too_many_arguments)]
fn log(
    root: &Root,
    spec: &str,
    source: LogSource,
    task: Option<&str>,
    contains: Option<&str>,
    skip: usize,
    limit: usize,
    limits: &NantyRead,
    secrets: &SecretSet,
    budget: &mut Budget,
) -> Result<Value, Failure> {
    let (file, max) = match source {
        LogSource::Dialog => ("dialog.jsonl", DIALOG_TEXT_CHARS),
        LogSource::Tools => (crate::server::events::TOOLS_TRAIL_FILE, TRAIL_TEXT_CHARS),
    };
    let rel = format!("{SPECS}/{spec}/{file}");
    let mut want = limits.file_bytes().min(LOG_MAX_BYTES);
    loop {
        let tail = root.tail(&rel, want)?;
        let items: Vec<LogItem> = parse_log(source, &whole_lines(&tail), secrets)
            .into_iter()
            .filter(|i| task.is_none_or(|t| i.task.as_deref() == Some(t)))
            .filter(|i| contains.is_none_or(|c| i.text.to_lowercase().contains(c)))
            .collect();
        let enough = items.len() >= skip.saturating_add(limit);
        if !(enough || tail.from_start || want >= LOG_MAX_BYTES) {
            want = want.saturating_mul(2).min(LOG_MAX_BYTES);
            continue;
        }
        let end = items.len().saturating_sub(skip);
        let start = end.saturating_sub(limit);
        let mut fit = Fit::new(budget);
        let mut out = Vec::new();
        let mut cut = false;
        for item in items[start..end].iter().rev() {
            let value = item.to_json(max);
            if !fit.take(&value) {
                cut = true;
                break;
            }
            out.push(value);
        }
        out.reverse();
        fit.close(budget);
        if out.is_empty() && cut {
            return Err(Failure::Read(ReadError::Budget));
        }
        // Há mais para trás: itens antes da janela, a cauda que não chegou ao começo ou o teto que cortou.
        let older = start > 0 || !tail.from_start || cut;
        return Ok(json!({ "spec": spec, "source": file, "items": out, "older": older }));
    }
}

/// A tarefa, interpretada: o frontmatter e o corpo. O texto ainda sem o redator: cada campo que sai é
/// redigido inteiro antes do corte.
fn task_of(root: &Root, spec: &str, task: &str, limits: &NantyRead) -> Result<crate::sdd::Task, Failure> {
    let text = root.read_raw(&format!("{SPECS}/{spec}/tasks/{task}.md"), limits)?;
    crate::sdd::Task::parse(&text).map_err(|_| Failure::Invalid)
}

/// As linhas finais de `text` que cabem em `room` bytes, com a primeira e a última (a partir de 1).
fn last_lines(text: &str, start: Option<usize>, lines: usize, room: usize) -> (String, usize, usize, usize, bool) {
    let all: Vec<&str> = text.lines().collect();
    let total = all.len();
    let (from, to) = match start {
        Some(s) => (s.min(total.saturating_add(1)), s.saturating_add(lines).saturating_sub(1).min(total)),
        None => (total.saturating_sub(lines) + 1, total),
    };
    let mut picked: Vec<&str> = Vec::new();
    let mut used = 0usize;
    let mut cut = from > 1 || to < total;
    // Do fim para trás: a última linha é a que mais importa na saída de um teste.
    for line in all.get(from.saturating_sub(1)..to).unwrap_or_default().iter().rev() {
        let need = line.len() + 1;
        if used + need > room {
            cut = true;
            break;
        }
        used += need;
        picked.push(line);
    }
    picked.reverse();
    let first = if picked.is_empty() { 0 } else { to + 1 - picked.len() };
    (picked.join("\n"), first, if picked.is_empty() { 0 } else { to }, total, cut)
}

#[allow(clippy::too_many_arguments)]
fn docs(
    root: &Root,
    spec: &str,
    part: Part,
    task: Option<&str>,
    start: Option<usize>,
    lines: Option<usize>,
    limits: &NantyRead,
    secrets: &SecretSet,
    budget: &mut Budget,
) -> Result<Value, Failure> {
    let range = LineRange { start: start.unwrap_or(1), count: lines };
    match part {
        Part::Specs => Err(Failure::Read(ReadError::Io)),
        Part::Spec => {
            let text = root.read(&format!("{SPECS}/{spec}/spec.md"), range, limits, secrets, budget)?;
            let mut value = text.to_json();
            value["spec"] = json!(spec);
            Ok(value)
        }
        Part::Tasks => tasks(root, spec, limits, secrets, budget),
        Part::Task => {
            let task = task.ok_or(Failure::NoTask)?;
            let text = root.read(&format!("{SPECS}/{spec}/tasks/{task}.md"), range, limits, secrets, budget)?;
            let mut value = text.to_json();
            value["spec"] = json!(spec);
            Ok(value)
        }
        Part::Tests => {
            let task = task.ok_or(Failure::NoTask)?;
            let parsed = task_of(root, spec, task, limits)?;
            let evidence = redact::scrub(secrets, parsed.frontmatter.test_evidence.as_deref().unwrap_or_default());
            let room = usize::try_from(budget.left()).unwrap_or(usize::MAX).saturating_sub(256);
            let want = lines.unwrap_or(TESTS_LINES).min(limits.range_lines());
            let (text, first, last, total, cut) = last_lines(&evidence, start, want, room);
            let value = json!({
                "spec": spec,
                "task": task,
                "status": parsed.frontmatter.status,
                "attempts": parsed.frontmatter.attempts,
                "max_attempts": parsed.frontmatter.max_attempts,
                "first_line": first,
                "last_line": last,
                "total_lines": total,
                "truncated": cut,
                "text": text,
            });
            spend_value(budget, &value);
            Ok(value)
        }
        Part::Recovery => {
            let task = task.ok_or(Failure::NoTask)?;
            let parsed = task_of(root, spec, task, limits)?;
            let notes = crate::agents::runner::recovery_notes(&parsed.body);
            let diagnosis = crate::agents::runner::diagnosis_notes(&parsed.body);
            let joined = [notes, diagnosis].into_iter().filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n\n");
            let room = usize::try_from(budget.left()).unwrap_or(usize::MAX).saturating_sub(256);
            // As análises mais novas ficam no fim: o corte, se houver, tira as mais antigas.
            let (text, _, _, _, cut) = last_lines(&redact::scrub(secrets, &joined), None, usize::MAX, room);
            let value = json!({ "spec": spec, "task": task, "truncated": cut, "text": text });
            spend_value(budget, &value);
            Ok(value)
        }
    }
}

fn spend_value(budget: &mut Budget, value: &Value) {
    budget.spend(serde_json::to_string(value).map(|s| s.len()).unwrap_or(0));
}

/// As tarefas da spec: o id, o título, o estado, as tentativas e o último veredito.
fn tasks(root: &Root, spec: &str, limits: &NantyRead, secrets: &SecretSet, budget: &mut Budget) -> Result<Value, Failure> {
    let mut scratch = Budget::new(limits);
    let listing = root.list(&format!("{SPECS}/{spec}/tasks"), limits, secrets, &mut scratch)?;
    let mut ids: Vec<String> = listing
        .entries
        .into_iter()
        .filter(|e| e.kind == files::EntryKind::File)
        .filter_map(|e| e.name.strip_suffix(".md").map(str::to_string))
        .filter(|id| is_task_id(id))
        .collect();
    ids.sort_by_key(|id| id[1..].parse::<u32>().unwrap_or(u32::MAX));
    let mut fit = Fit::new(budget);
    let mut out = Vec::new();
    let mut cut = ids.len() > MAX_TASKS;
    for id in ids.iter().take(MAX_TASKS) {
        let item = match task_of(root, spec, id, limits) {
            Ok(t) => {
                let (title, _) = clean(secrets, &t.frontmatter.title, 200);
                json!({
                    "task": id,
                    "title": title,
                    "status": t.frontmatter.status,
                    "attempts": t.frontmatter.attempts,
                    "max_attempts": t.frontmatter.max_attempts,
                    "last_review_verdict": t.frontmatter.last_verdict,
                })
            }
            Err(f) => json!({ "task": id, "error": f.code() }),
        };
        if !fit.take(&item) {
            cut = true;
            break;
        }
        out.push(item);
    }
    fit.close(budget);
    if out.is_empty() && cut {
        return Err(Failure::Read(ReadError::Budget));
    }
    Ok(json!({ "spec": spec, "tasks": out, "truncated": cut }))
}

/// Um arquivo por faixa ou, se for pasta (ou a raiz), a listagem.
fn files_read(root: &Root, path: &str, start: Option<usize>, lines: Option<usize>, limits: &NantyRead, secrets: &SecretSet, budget: &mut Budget) -> Result<Value, Failure> {
    let path = path.trim_matches('/');
    if path.is_empty() || path == "." {
        return Ok(root.list("", limits, secrets, budget)?.to_json());
    }
    match root.read(path, LineRange { start: start.unwrap_or(1), count: lines }, limits, secrets, budget) {
        Ok(text) => Ok(text.to_json()),
        Err(ReadError::NotFile) => Ok(root.list(path, limits, secrets, budget)?.to_json()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
#[path = "deep_tests.rs"]
mod tests;
