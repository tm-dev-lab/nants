//! Persistência e agregação do uso de tokens (`usage.jsonl`) e `GET /api/projects/:id/usage`.
//!
//! Cada chamada ao modelo vira uma linha JSON em `.sdd/specs/<spec>/usage.jsonl` (chat sem spec:
//! `.sdd/specs/.usage.jsonl`). Os arquivos não são versionados (exclude local) e têm limite de
//! tamanho: passado o limite, novas linhas são descartadas. O custo só existe quando o usuário
//! configurou preço para o modelo; nunca há preço presumido.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxPath, Query, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::error::ApiError;
use super::events::validate_project_id;
use super::projects::{find_project, format_utc};
use super::safe_fs::{self, SafeFsError};
use super::spec_result::validate_spec_id;
use super::state::AppState;
use crate::agents::usage::{BudgetGuard, UsageEvent, UsageScope, UsageSink};
use crate::config::{PriceSource, Pricing};

/// Limite de tamanho de cada `usage.jsonl` (4 MiB).
pub const MAX_USAGE_BYTES: u64 = 4 * 1024 * 1024;
const SPECS_DIR: &str = ".sdd/specs";
const USAGE_FILE: &str = "usage.jsonl";
/// Arquivo das chamadas sem spec (chat).
const NO_SPEC_FILE: &str = ".sdd/specs/.usage.jsonl";

/// Qualidades de imagem aceitas no registro (spec 092 T03).
const IMAGE_QUALITIES: [&str; 3] = ["low", "medium", "high"];

/// Uma linha de `usage.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub at: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    pub agent: String,
    pub model: String,
    pub input: u64,
    pub output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// Chamadas de busca na web da resposta; ausente = 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_calls: Option<u64>,
    /// Imagens geradas (spec 090); ausente = 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<u64>,
    /// Qualidade das imagens geradas (`low|medium|high`, spec 092 T03); ausente = `medium`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_quality: Option<String>,
}

/// Sink que anexa ao `usage.jsonl` do projeto. Falhas de I/O são ignoradas (não derrubam o agente).
pub struct FileUsageSink {
    root: PathBuf,
    project: String,
}

impl FileUsageSink {
    pub fn new(root: impl Into<PathBuf>, project: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { root: root.into(), project: project.into() })
    }

    /// Escopo de registro para um agente do projeto.
    pub fn scope(
        root: &Path,
        project: &str,
        agent: &str,
        spec: Option<String>,
        task: Option<String>,
    ) -> UsageScope {
        UsageScope { sink: Self::new(root, project), agent: agent.to_string(), spec, task }
    }
}

impl UsageSink for FileUsageSink {
    fn record(&self, event: UsageEvent) {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let spec = event.spec.filter(|s| validate_spec_id(s).is_ok());
        let rel = match &spec {
            Some(s) => format!("{SPECS_DIR}/{s}/{USAGE_FILE}"),
            None => NO_SPEC_FILE.to_string(),
        };
        let record = UsageRecord {
            at: format_utc(secs),
            project: self.project.clone(),
            spec,
            task: event.task,
            agent: event.agent,
            model: event.model,
            input: u64::from(event.usage.prompt),
            output: u64::from(event.usage.completion),
            cached: event.usage.cached.map(u64::from),
            reasoning: event.usage.reasoning.map(u64::from),
            web_search_calls: (event.web_search_calls > 0).then_some(u64::from(event.web_search_calls)),
            images: (event.images > 0).then_some(u64::from(event.images)),
            image_quality: event
                .image_quality
                .filter(|q| event.images > 0 && IMAGE_QUALITIES.contains(&q.as_str())),
        };
        if let Ok(line) = serde_json::to_string(&record) {
            let _ = safe_fs::append_bounded(&self.root, &rel, &line, MAX_USAGE_BYTES);
        }
    }
}

/// Totais de um grupo de chamadas.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Totals {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub reasoning: u64,
    /// Soma em US$ das chamadas com preço configurado; `null` se nenhuma tem preço.
    pub cost_usd: Option<f64>,
    /// Chamadas sem preço para o modelo (o custo acima as exclui).
    pub unpriced_calls: u64,
    /// `true` se alguma chamada somada usou preço estimado (catálogo), não digitado.
    pub cost_estimated: bool,
    /// Chamadas de busca na web somadas (omitido quando zero).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub web_search_calls: u64,
    /// US$ das buscas na web (já incluído em `cost_usd`; omitido quando zero).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub web_search_usd: f64,
    /// Imagens geradas somadas (spec 092 T03; omitido quando zero).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub images: u64,
    /// US$ das imagens (já incluído em `cost_usd`; omitido quando zero).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub images_usd: f64,
    /// Alguma imagem somada usou a estimativa padrão por qualidade (sem `image_per_unit`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub images_estimated: bool,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

impl Totals {
    fn add(&mut self, r: &UsageRecord, pricing: &BTreeMap<String, Pricing>) {
        self.calls += 1;
        self.input += r.input;
        self.output += r.output;
        self.cached += r.cached.unwrap_or(0);
        self.reasoning += r.reasoning.unwrap_or(0);
        let searches = r.web_search_calls.unwrap_or(0);
        self.web_search_calls += searches;
        if let Some(p) = pricing.get(&r.model) {
            self.web_search_usd += searches as f64 * p.web_search_price();
        }
        let image = image_cost(r, pricing);
        if let Some((usd, estimated)) = image {
            self.images += r.images.unwrap_or(0);
            self.images_usd += usd;
            self.images_estimated |= estimated;
        }
        match cost_of(r, pricing) {
            Some(c) => {
                self.cost_usd = Some(self.cost_usd.unwrap_or(0.0) + c);
                if pricing.get(&r.model).is_some_and(|p| p.source == PriceSource::Estimated)
                    || image.is_some_and(|(_, estimated)| estimated)
                {
                    self.cost_estimated = true;
                }
            }
            None => self.unpriced_calls += 1,
        }
    }
}

/// Custo em US$ das imagens de um registro e se ele é estimado (spec 092 T03): `images` ×
/// (`image_per_unit` do modelo ou a estimativa padrão pela qualidade gravada). `None` sem imagens.
/// Sem preço para o modelo, vale a estimativa padrão.
pub fn image_cost(r: &UsageRecord, pricing: &BTreeMap<String, Pricing>) -> Option<(f64, bool)> {
    let count = r.images.filter(|n| *n > 0)?;
    let quality = r.image_quality.as_deref();
    let (unit, estimated) = match pricing.get(&r.model) {
        Some(p) => p.image_price(quality),
        None => (crate::config::default_image_price(quality), true),
    };
    Some((count as f64 * unit, estimated))
}

/// Custo em US$ de uma chamada, ou `None` sem preço para o modelo (salvo registro com imagens: aí
/// conta só a estimativa das imagens). A entrada em cache usa `cached_input_per_mtok` quando houver;
/// senão, o preço de entrada. As imagens ([`image_cost`]) somam ao custo dos tokens.
pub fn cost_of(r: &UsageRecord, pricing: &BTreeMap<String, Pricing>) -> Option<f64> {
    let images = image_cost(r, pricing).map(|(usd, _)| usd);
    let Some(p) = pricing.get(&r.model) else {
        return images;
    };
    let cached = r.cached.unwrap_or(0).min(r.input);
    let cached_price = p.cached_input_per_mtok.unwrap_or(p.input_per_mtok);
    let total = (r.input - cached) as f64 * p.input_per_mtok
        + cached as f64 * cached_price
        + r.output as f64 * p.output_per_mtok;
    let web = r.web_search_calls.unwrap_or(0) as f64 * p.web_search_price();
    Some(total / 1_000_000.0 + web + images.unwrap_or(0.0))
}

#[derive(Debug, Serialize, PartialEq)]
pub struct NamedTotals {
    pub id: String,
    #[serde(flatten)]
    pub totals: Totals,
}

/// Totais de uma tarefa com o tempo de trabalho (086/T06; campos aditivos).
#[derive(Debug, Serialize, PartialEq)]
pub struct TaskTotals {
    pub id: String,
    #[serde(flatten)]
    pub totals: Totals,
    /// Primeiro registro da tarefa (uso ou ação), ISO UTC; `null` sem instante legível.
    pub first_at: Option<String>,
    /// Último registro da tarefa (uso ou ação), ISO UTC.
    pub last_at: Option<String>,
    /// Soma dos intervalos entre registros consecutivos, sem os maiores que `MAX_GAP_SECS`.
    pub active_secs: u64,
}

/// Intervalo maior que isto entre dois registros da tarefa é pausa e não conta (10 min).
pub const MAX_GAP_SECS: u64 = 10 * 60;

/// Instantes ordenados → (primeiro, último, segundos ativos sem as pausas longas).
fn active_time(marks: &mut [u64]) -> Option<(u64, u64, u64)> {
    marks.sort_unstable();
    let (first, last) = (*marks.first()?, *marks.last()?);
    let active = marks
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|gap| *gap <= MAX_GAP_SECS)
        .sum();
    Some((first, last, active))
}

/// Lê "AAAA-MM-DDTHH:MM:SSZ" (o formato de `format_utc`) como segundos Unix; outro formato → `None`.
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        if !part.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, min, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    // Dias civis → dias desde 1970-01-01 (inverso do algoritmo de `format_utc`).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + min * 60 + sec).ok()
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SpecUsage {
    /// `null` para chamadas sem spec (chat).
    pub spec: Option<String>,
    #[serde(flatten)]
    pub totals: Totals,
    pub tasks: Vec<TaskTotals>,
    pub agents: Vec<NamedTotals>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct UsageView {
    pub specs: Vec<SpecUsage>,
    pub total: Totals,
    /// Totais por modelo: cada chamada é precificada pelo próprio modelo e somadas.
    pub by_model: Vec<NamedTotals>,
    pub by_agent: Vec<NamedTotals>,
    /// Totais por spec (chamadas sem spec: id `chat`); vazio quando a consulta filtra uma spec.
    pub by_spec: Vec<NamedTotals>,
    /// Orçamento da spec consultada (`?spec=`), quando definido.
    pub budget: Option<Budget>,
}

/// Agrega registros por spec, tarefa e agente (sem a trilha de ações).
pub fn aggregate(records: &[UsageRecord], pricing: &BTreeMap<String, Pricing>) -> UsageView {
    aggregate_with_tools(records, &BTreeMap::new(), pricing)
}

/// Ações da trilha por spec: (`task_id`, `ts` em segundos Unix).
pub type ToolMarks = BTreeMap<String, Vec<(String, u64)>>;

/// Agrega registros por spec, tarefa e agente. O tempo de cada tarefa junta os instantes do uso
/// (`at`) e das ações (`tools`); tarefa só com ações aparece com totais zerados.
pub fn aggregate_with_tools(
    records: &[UsageRecord],
    tools: &ToolMarks,
    pricing: &BTreeMap<String, Pricing>,
) -> UsageView {
    type Group = (Totals, BTreeMap<String, Totals>, BTreeMap<String, Totals>);
    let mut by_spec: BTreeMap<Option<String>, Group> = BTreeMap::new();
    let mut total = Totals::default();
    let mut models: BTreeMap<String, Totals> = BTreeMap::new();
    let mut agents: BTreeMap<String, Totals> = BTreeMap::new();
    let mut specs: BTreeMap<String, Totals> = BTreeMap::new();
    for r in records {
        total.add(r, pricing);
        models.entry(r.model.clone()).or_default().add(r, pricing);
        agents.entry(r.agent.clone()).or_default().add(r, pricing);
        specs.entry(r.spec.clone().unwrap_or_else(|| "chat".to_string())).or_default().add(r, pricing);
        let g = by_spec.entry(r.spec.clone()).or_default();
        g.0.add(r, pricing);
        if let Some(t) = &r.task {
            g.1.entry(t.clone()).or_default().add(r, pricing);
        }
        g.2.entry(r.agent.clone()).or_default().add(r, pricing);
    }
    let named = |m: BTreeMap<String, Totals>| {
        m.into_iter().map(|(id, totals)| NamedTotals { id, totals }).collect()
    };
    // Instantes por (spec, tarefa): uso + ações.
    let mut marks: BTreeMap<(String, String), Vec<u64>> = BTreeMap::new();
    for r in records {
        if let (Some(spec), Some(task), Some(at)) = (&r.spec, &r.task, parse_utc(&r.at)) {
            marks.entry((spec.clone(), task.clone())).or_default().push(at);
        }
    }
    for (spec, items) in tools {
        if !by_spec.contains_key(&Some(spec.clone())) {
            continue;
        }
        for (task, ts) in items {
            marks.entry((spec.clone(), task.clone())).or_default().push(*ts);
            if let Some(g) = by_spec.get_mut(&Some(spec.clone())) {
                g.1.entry(task.clone()).or_default();
            }
        }
    }
    UsageView {
        specs: by_spec
            .into_iter()
            .map(|(spec, (totals, tasks, agents))| {
                let tasks = tasks
                    .into_iter()
                    .map(|(id, totals)| {
                        let time = spec
                            .as_ref()
                            .and_then(|s| marks.get_mut(&(s.clone(), id.clone())))
                            .and_then(|m| active_time(m));
                        TaskTotals {
                            first_at: time.map(|(f, _, _)| format_utc(f)),
                            last_at: time.map(|(_, l, _)| format_utc(l)),
                            active_secs: time.map_or(0, |(_, _, a)| a),
                            id,
                            totals,
                        }
                    })
                    .collect();
                SpecUsage { spec, totals, tasks, agents: named(agents) }
            })
            .collect(),
        total,
        by_model: named(models),
        by_agent: named(agents),
        by_spec: named(specs),
        budget: None,
    }
}

/// Linhas válidas de um arquivo; ausente ou ilegível → vazio. Linhas inválidas são ignoradas.
fn read_records(root: &Path, rel: &str) -> Vec<UsageRecord> {
    match safe_fs::read_regular(root, rel, MAX_USAGE_BYTES + 4096) {
        Ok(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
        Err(SafeFsError::Missing) | Err(_) => Vec::new(),
    }
}

/// Lê o uso de uma spec ou, sem filtro, de todas (inclusive o arquivo sem spec).
pub fn load_records(root: &Path, spec: Option<&str>) -> Vec<UsageRecord> {
    if let Some(s) = spec {
        return read_records(root, &format!("{SPECS_DIR}/{s}/{USAGE_FILE}"));
    }
    let mut out = read_records(root, NO_SPEC_FILE);
    let Ok(dir) = safe_fs::check_chain(root, SPECS_DIR) else {
        return out;
    };
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| validate_spec_id(n).is_ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    for n in names {
        out.extend(read_records(root, &format!("{SPECS_DIR}/{n}/{USAGE_FILE}")));
    }
    out
}

/// Ações da trilha (`tools.jsonl`, via `safe_fs`) da spec consultada; sem filtro, vazio.
pub fn load_tool_marks(root: &Path, spec: Option<&str>) -> ToolMarks {
    // Sem filtro (indicador do projeto), ninguém usa o tempo por tarefa: a trilha não é lida,
    // para a recarga ao vivo não somar I/O que cresce com o histórico (F-01).
    let Some(s) = spec.filter(|s| validate_spec_id(s).is_ok()) else {
        return ToolMarks::new();
    };
    let marks = super::tools_trail::task_timestamps(root, s);
    if marks.is_empty() {
        return ToolMarks::new();
    }
    ToolMarks::from([(s.to_string(), marks)])
}

/// Orçamento opcional de uma spec (estado local em `budget.json`, fora do `spec.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_usd: Option<f64>,
}

impl Budget {
    fn is_valid(&self) -> bool {
        self.max_usd.map_or(true, |v| v.is_finite() && v >= 0.0)
    }

    fn is_empty(&self) -> bool {
        self.max_tokens.is_none() && self.max_usd.is_none()
    }
}

const BUDGET_FILE: &str = "budget.json";

fn budget_rel(spec: &str) -> String {
    format!("{SPECS_DIR}/{spec}/{BUDGET_FILE}")
}

/// Orçamento gravado da spec; ausente, inválido ou ilegível → `None`.
pub fn read_budget(root: &Path, spec: &str) -> Option<Budget> {
    if validate_spec_id(spec).is_err() {
        return None;
    }
    let text = safe_fs::read_regular(root, &budget_rel(spec), 4096).ok()?;
    let b: Budget = serde_json::from_str(&text).ok()?;
    (b.is_valid() && !b.is_empty()).then_some(b)
}

fn write_budget(root: &Path, spec: &str, budget: &Budget) -> std::io::Result<()> {
    let rel = budget_rel(spec);
    let text = serde_json::to_string(budget).map_err(std::io::Error::other)?;
    let bad = |_| std::io::Error::other("caminho inseguro");
    safe_fs::ensure_dirs(root, &format!("{SPECS_DIR}/{spec}")).map_err(bad)?;
    if safe_fs::write_new(root, &rel, &text).map_err(bad)? {
        return Ok(());
    }
    super::approvals::replace_atomic(root, &rel, text.as_bytes())
}

/// Motivo de pausa quando o uso da spec atingiu o orçamento; `None` se não há orçamento ou há folga.
pub fn budget_status(root: &Path, spec: &str, pricing: &BTreeMap<String, Pricing>) -> Option<String> {
    let budget = read_budget(root, spec)?;
    let view = aggregate(&load_records(root, Some(spec)), pricing);
    let tokens = view.total.input + view.total.output;
    if let Some(max) = budget.max_tokens {
        if tokens >= max {
            return Some(format!("orçamento da spec atingido: {tokens} de {max} tokens"));
        }
    }
    if let (Some(max), Some(cost)) = (budget.max_usd, view.total.cost_usd) {
        if cost >= max {
            return Some(format!("orçamento da spec atingido: US$ {cost:.2} de US$ {max:.2}"));
        }
    }
    None
}

/// Guarda de orçamento do runner: relê o uso e o limite a cada consulta.
pub struct FileBudgetGuard {
    root: PathBuf,
    pricing: BTreeMap<String, Pricing>,
}

impl FileBudgetGuard {
    pub fn new(root: impl Into<PathBuf>, pricing: BTreeMap<String, Pricing>) -> Arc<Self> {
        Arc::new(Self { root: root.into(), pricing })
    }
}

impl BudgetGuard for FileBudgetGuard {
    fn exceeded(&self, spec: &str) -> Option<String> {
        budget_status(&self.root, spec, &self.pricing)
    }
}

/// `PUT /api/projects/:id/specs/:spec/budget`: define o orçamento; corpo vazio `{}` remove.
pub async fn put_budget(
    State(state): State<AppState>,
    AxPath((id, spec)): AxPath<(String, String)>,
    body: axum::body::Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    validate_spec_id(&spec)?;
    let budget: Budget = serde_json::from_slice(&body)
        .map_err(|_| ApiError::unprocessable("corpo JSON com campos inválidos"))?;
    if !budget.is_valid() {
        return Err(ApiError::unprocessable("orçamento inválido: use números finitos maiores ou iguais a zero"));
    }
    let project = find_project(&state, &id).await?;
    let root = PathBuf::from(&project.path);
    tokio::task::spawn_blocking(move || {
        if budget.is_empty() {
            return safe_fs::remove_regular(&root, &budget_rel(&spec))
                .map_err(|_| ApiError::internal("falha ao remover o orçamento"));
        }
        if !root.join(SPECS_DIR).join(&spec).is_dir() {
            return Err(ApiError::new(axum::http::StatusCode::NOT_FOUND, "spec não encontrada"));
        }
        write_budget(&root, &spec, &budget).map_err(|_| ApiError::internal("falha ao gravar o orçamento"))
    })
    .await
    .map_err(|_| ApiError::internal("falha interna"))??;
    Ok(Json(budget).into_response())
}

#[derive(Deserialize)]
pub struct UsageQuery {
    spec: Option<String>,
}

/// `GET /api/projects/:id/usage?spec=<id>`
pub async fn get_usage(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<UsageQuery>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    if let Some(s) = &q.spec {
        validate_spec_id(s)?;
    }
    let project = find_project(&state, &id).await?;
    let pricing = state.config().read().await.pricing.clone();
    let root = PathBuf::from(&project.path);
    let view = tokio::task::spawn_blocking(move || {
        let records = load_records(&root, q.spec.as_deref());
        let tools = load_tool_marks(&root, q.spec.as_deref());
        let mut view = aggregate_with_tools(&records, &tools, &pricing);
        if q.spec.is_some() {
            view.by_spec.clear();
        }
        view.budget = q.spec.as_deref().and_then(|s| read_budget(&root, s));
        view
    })
    .await
    .map_err(|_| ApiError::internal("falha interna"))?;
    Ok(Json(view).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm_client::Usage;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("nants-usage-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(".sdd/specs/s1")).expect("mkdir");
        d
    }

    fn ev(spec: Option<&str>, task: Option<&str>, agent: &str, model: &str, u: Usage) -> UsageEvent {
        UsageEvent {
            agent: agent.into(),
            model: model.into(),
            spec: spec.map(String::from),
            task: task.map(String::from),
            usage: u,
            web_search_calls: 0,
            images: 0,
            image_quality: None,
        }
    }

    fn price(i: f64, o: f64, c: Option<f64>) -> BTreeMap<String, Pricing> {
        BTreeMap::from([("m".to_string(), Pricing { input_per_mtok: i, output_per_mtok: o, cached_input_per_mtok: c, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None })])
    }

    #[test]
    fn totals_sum_web_search_calls_and_usd_and_omit_when_zero() {
        let mk = |w: Option<u64>| UsageRecord {
            at: "t".into(),
            project: "p".into(),
            spec: None,
            task: None,
            agent: "architect".into(),
            model: "m".into(),
            input: 10,
            output: 5,
            cached: None,
            reasoning: None,
            web_search_calls: w,
            images: None,
            image_quality: None,
        };
        let pr = BTreeMap::from([("m".to_string(), Pricing { input_per_mtok: 0.0, output_per_mtok: 0.0, cached_input_per_mtok: None, web_search_per_call: Some(0.5), image_per_unit: None, source: Default::default(), estimated_at: None })]);
        let v = aggregate(&[mk(Some(2)), mk(Some(1)), mk(None)], &pr);
        assert_eq!(v.total.web_search_calls, 3);
        assert!((v.total.web_search_usd - 1.5).abs() < 1e-9);
        let json = serde_json::to_value(&v.total).expect("json");
        assert_eq!(json["web_search_calls"], 3);
        let none = aggregate(&[mk(None)], &pr);
        let json = serde_json::to_value(&none.total).expect("json");
        assert!(json.get("web_search_calls").is_none() && json.get("web_search_usd").is_none());
    }

    #[test]
    fn sink_writes_per_spec_and_chat_file_and_aggregates() {
        let root = tmp("agg");
        let sink = FileUsageSink::new(&root, "p1");
        sink.record(ev(Some("s1"), Some("T01"), "developer", "m", Usage { prompt: 1000, completion: 500, cached: Some(400), reasoning: Some(100) }));
        sink.record(ev(Some("s1"), Some("T01"), "reviewer", "m", Usage::new(10, 5)));
        sink.record(ev(Some("s1"), Some("T02"), "developer", "other", Usage::new(7, 3)));
        sink.record(ev(None, None, "architect", "m", Usage::new(2, 1)));
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("file");
        assert_eq!(text.lines().count(), 3);
        assert!(root.join(".sdd/specs/.usage.jsonl").is_file());
        let first: UsageRecord = serde_json::from_str(text.lines().next().expect("l")).expect("json");
        assert_eq!((first.project.as_str(), first.input, first.cached), ("p1", 1000, Some(400)));

        let only = aggregate(&load_records(&root, Some("s1")), &price(2.0, 8.0, Some(0.5)));
        assert_eq!(only.specs.len(), 1);
        let s = &only.specs[0];
        assert_eq!((s.totals.input, s.totals.output, s.totals.calls), (1017, 508, 3));
        assert_eq!(s.tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["T01", "T02"]);
        assert_eq!(s.agents.len(), 2);
        // T01 dev: 600*2 + 400*0.5 + 500*8 = 5400 -> 0.0054; reviewer: 10*2+5*8 = 60 -> 0.00006
        let cost = s.totals.cost_usd.expect("custo");
        assert!((cost - 0.00546).abs() < 1e-9, "{cost}");
        assert_eq!(s.totals.unpriced_calls, 1, "modelo sem preço fica fora do custo");

        // Dois modelos com preços distintos: o custo total é a soma dos custos por modelo.
        let two = BTreeMap::from([
            ("m".to_string(), Pricing { input_per_mtok: 2.0, output_per_mtok: 8.0, cached_input_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None }),
            ("other".to_string(), Pricing { input_per_mtok: 100.0, output_per_mtok: 200.0, cached_input_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None }),
        ]);
        let v = aggregate(&load_records(&root, None), &two);
        assert_eq!(v.by_model.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m", "other"]);
        let sum: f64 = v.by_model.iter().filter_map(|m| m.totals.cost_usd).sum();
        assert!((v.total.cost_usd.expect("custo") - sum).abs() < 1e-12);
        let other = v.by_model.iter().find(|m| m.id == "other").and_then(|m| m.totals.cost_usd).expect("c");
        assert!((other - (7.0 * 100.0 + 3.0 * 200.0) / 1e6).abs() < 1e-12);
        assert!(v.by_spec.iter().any(|s| s.id == "chat") && v.by_spec.iter().any(|s| s.id == "s1"));
        assert!(v.by_agent.iter().any(|a| a.id == "architect"));

        let all = aggregate(&load_records(&root, None), &BTreeMap::new());
        assert_eq!(all.specs.len(), 2);
        assert_eq!(all.total.calls, 4);
        assert_eq!(all.total.cost_usd, None, "sem preço, sem custo");
        assert!(all.specs.iter().any(|s| s.spec.is_none()));
    }

    #[test]
    fn custo_com_preco_estimado_leva_o_selo() {
        let root = tmp("est");
        let sink = FileUsageSink::new(&root, "p1");
        sink.record(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(10, 5)));
        let mut pricing = price(1.0, 1.0, None);
        let manual = aggregate(&load_records(&root, Some("s1")), &pricing);
        assert!(!manual.total.cost_estimated);
        if let Some(p) = pricing.get_mut("m") {
            p.source = PriceSource::Estimated;
        }
        let est = aggregate(&load_records(&root, Some("s1")), &pricing);
        assert!(est.total.cost_estimated);
    }

    #[test]
    fn cached_without_cached_price_bills_as_input() {
        let r = UsageRecord {
            at: String::new(), project: String::new(), spec: None, task: None,
            agent: "a".into(), model: "m".into(), input: 1000, output: 0, cached: Some(400), reasoning: None, web_search_calls: None, images: None, image_quality: None,
        };
        let c = cost_of(&r, &price(1.0, 1.0, None)).expect("preço");
        assert!((c - 0.001).abs() < 1e-12);
        assert_eq!(cost_of(&r, &BTreeMap::new()), None);
    }

    /// Spec 092 T03: imagens × (image_per_unit ou estimativa por qualidade), somadas aos tokens.
    #[test]
    fn image_cost_uses_unit_price_or_the_default_per_quality() {
        let img = |n: u64, q: Option<&str>| UsageRecord {
            at: String::new(), project: String::new(), spec: Some("s1".into()), task: Some("T01".into()),
            agent: "developer".into(), model: "m".into(), input: 1_000_000, output: 0, cached: None, reasoning: None,
            web_search_calls: None, images: Some(n), image_quality: q.map(String::from),
        };
        // Sem image_per_unit: padrão por qualidade, estimado; soma ao custo dos tokens (US$ 1).
        let pr = price(1.0, 1.0, None);
        for (q, unit) in [(Some("low"), 0.02), (Some("medium"), 0.06), (Some("high"), 0.25), (None, 0.06)] {
            let (usd, est) = image_cost(&img(2, q), &pr).expect("imagens");
            assert!((usd - 2.0 * unit).abs() < 1e-12 && est, "{q:?}");
            let c = cost_of(&img(2, q), &pr).expect("preço");
            assert!((c - (1.0 + 2.0 * unit)).abs() < 1e-12, "{q:?}: {c}");
        }
        // Com image_per_unit: o valor do usuário, sem estimativa.
        let mut set = price(1.0, 1.0, None);
        set.get_mut("m").expect("m").image_per_unit = Some(0.1);
        let (usd, est) = image_cost(&img(3, Some("high")), &set).expect("imagens");
        assert!((usd - 0.3).abs() < 1e-12 && !est);
        let v = aggregate(&[img(3, Some("high"))], &set);
        assert_eq!((v.total.images, v.total.images_estimated, v.total.cost_estimated), (3, false, false));
        assert!((v.total.images_usd - 0.3).abs() < 1e-12 && (v.total.cost_usd.expect("c") - 1.3).abs() < 1e-12);
        // Agregado com estimativa: marcado como estimado (custo e imagens), na tarefa também.
        let v = aggregate(&[img(1, Some("low")), img(2, Some("high"))], &pr);
        assert_eq!((v.total.images, v.total.images_estimated, v.total.cost_estimated), (3, true, true));
        assert!((v.total.images_usd - 0.52).abs() < 1e-12);
        let task = &v.specs[0].tasks[0].totals;
        assert!((task.cost_usd.expect("tarefa") - 2.52).abs() < 1e-12 && task.images == 3);
        let json = serde_json::to_value(&v.total).expect("json");
        assert_eq!((json["images"].as_u64(), json["images_estimated"].as_bool()), (Some(3), Some(true)));
        // Sem preço do modelo: só a estimativa das imagens (os tokens ficam sem custo).
        let none = aggregate(&[img(1, Some("medium"))], &BTreeMap::new());
        assert!((none.total.cost_usd.expect("estimativa") - 0.06).abs() < 1e-12 && none.total.cost_estimated);
        // Sem imagens: nada muda e os campos somem do JSON.
        let mut plain = img(0, None);
        plain.images = None;
        assert_eq!(image_cost(&plain, &pr), None);
        let json = serde_json::to_value(&aggregate(&[plain], &pr).total).expect("json");
        assert!(json.get("images").is_none() && json.get("images_usd").is_none() && json.get("images_estimated").is_none());
    }

    #[test]
    fn image_quality_is_recorded_only_with_images_and_valid_values() {
        let root = tmp("quality");
        let mut e = ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1, 1));
        e.images = 1;
        e.image_quality = Some("high".into());
        FileUsageSink::new(&root, "p").record(e.clone());
        e.image_quality = Some("ultra".into());
        FileUsageSink::new(&root, "p").record(e.clone());
        e.images = 0;
        e.image_quality = Some("low".into());
        FileUsageSink::new(&root, "p").record(e);
        let recs = load_records(&root, Some("s1"));
        let qs: Vec<Option<&str>> = recs.iter().map(|r| r.image_quality.as_deref()).collect();
        assert_eq!(qs, [Some("high"), None, None]);
    }

    #[test]
    fn size_limit_drops_new_lines() {
        let root = tmp("limit");
        let rel = ".sdd/specs/s1/usage.jsonl";
        assert_eq!(safe_fs::append_bounded(&root, rel, "abcd", 9), Ok(true));
        assert_eq!(safe_fs::append_bounded(&root, rel, "abcd", 9), Ok(false));
        assert_eq!(fs::read_to_string(root.join(rel)).expect("r"), "abcd\n");
    }

    #[test]
    fn without_sdd_dir_nothing_is_created() {
        let root = std::env::temp_dir().join(format!("nants-usage-{}-nosdd", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("mkdir");
        FileUsageSink::new(&root, "p").record(ev(None, None, "architect", "m", Usage::new(1, 1)));
        assert!(!root.join(".sdd").exists());
    }

    #[test]
    fn invalid_spec_id_falls_back_to_chat_file() {
        let root = tmp("badspec");
        FileUsageSink::new(&root, "p").record(ev(Some("../x"), None, "a", "m", Usage::new(1, 1)));
        assert!(root.join(".sdd/specs/.usage.jsonl").is_file());
    }

    #[tokio::test]
    async fn removing_budget_never_follows_symlinked_spec_dir() {
        use crate::config::{Config, Project};
        let root = tmp("rm-sym");
        let outside = tmp("rm-sym-out");
        fs::write(outside.join("budget.json"), "{}").expect("w");
        fs::create_dir_all(root.join(".sdd/specs")).expect("d");
        let _ = fs::remove_dir(root.join(".sdd/specs/s1"));
        std::os::unix::fs::symlink(&outside, root.join(".sdd/specs/s1")).expect("ln");
        let cfg = Config {
            projects: vec![Project {
                id: "p1".into(),
                name: "P".into(),
                path: root.to_string_lossy().into_owned(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            ..Config::default()
        };
        let state = AppState::new(tmp("rm-sym-cfg").join("config.toml"), cfg, 3000);
        let r = put_budget(State(state), AxPath(("p1".into(), "s1".into())), axum::body::Bytes::from("{}")).await;
        assert!(r.is_err());
        assert!(outside.join("budget.json").exists());
    }

    #[test]
    fn budget_status_by_tokens_and_usd_and_removal() {
        let root = tmp("budget");
        fs::create_dir_all(root.join(".sdd/specs/s1")).expect("dir");
        let sink = FileUsageSink::new(&root, "p");
        sink.record(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1_000_000, 0)));
        assert!(budget_status(&root, "s1", &BTreeMap::new()).is_none(), "sem orçamento");
        write_budget(&root, "s1", &Budget { max_tokens: Some(2_000_000), max_usd: Some(2.0) }).expect("w");
        assert!(budget_status(&root, "s1", &BTreeMap::new()).is_none(), "tokens abaixo; sem preço não há US$");
        let p = price(3.0, 0.0, None);
        let r = budget_status(&root, "s1", &p).expect("US$ atingido");
        assert_eq!(r, "orçamento da spec atingido: US$ 3.00 de US$ 2.00");
        write_budget(&root, "s1", &Budget { max_tokens: Some(1_000_000), max_usd: None }).expect("w2");
        let r = budget_status(&root, "s1", &BTreeMap::new()).expect("tokens atingido");
        assert_eq!(r, "orçamento da spec atingido: 1000000 de 1000000 tokens");
        assert_eq!(FileBudgetGuard::new(&root, BTreeMap::new()).exceeded("s1"), Some(r));
        assert!(!Budget { max_tokens: None, max_usd: Some(-1.0) }.is_valid());
    }

    #[tokio::test]
    async fn endpoints_pricing_put_and_usage_get() {
        use axum::body::Bytes;
        use crate::config::{Config, Project};
        let root = tmp("api");
        let dir = tmp("api-cfg");
        let cfg = Config {
            projects: vec![Project {
                id: "p1".into(),
                name: "P".into(),
                path: root.to_string_lossy().into_owned(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            ..Config::default()
        };
        let state = AppState::new(dir.join("config.toml"), cfg, 3000);
        FileUsageSink::new(&root, "p1").record(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1_000_000, 0)));

        let get = |spec: Option<&str>| {
            get_usage(State(state.clone()), AxPath("p1".into()), Query(UsageQuery { spec: spec.map(String::from) }))
        };
        let body = |r: Response| async move {
            let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.expect("body");
            serde_json::from_slice::<serde_json::Value>(&b).expect("json")
        };
        let v = body(get(Some("s1")).await.expect("ok")).await;
        assert_eq!(v["total"]["input"], 1_000_000);
        assert!(v["total"]["cost_usd"].is_null(), "sem preço, custo nulo");

        let bad = |json: &str| super::super::config_api::put_pricing(State(state.clone()), Bytes::from(json.to_string()));
        assert!(bad(r#"{"m":{"input_per_mtok":-1,"output_per_mtok":1}}"#).await.is_err());
        assert!(bad(r#"{"m":{"input_per_mtok":"x","output_per_mtok":1}}"#).await.is_err());
        assert!(state.config().read().await.pricing.is_empty());
        bad(r#"{"m":{"input_per_mtok":3,"output_per_mtok":15}}"#).await.expect("ok");
        assert!(dir.join("config.toml").is_file());
        // Spec 092 T03: a rota soma o custo das imagens (estimativa padrão) e o marca.
        let mut e = ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(0, 0));
        e.images = 2;
        e.image_quality = Some("high".into());
        FileUsageSink::new(&root, "p1").record(e);
        let v = body(get(Some("s1")).await.expect("ok")).await;
        assert_eq!((v["total"]["images"].as_u64(), v["total"]["images_estimated"].as_bool()), (Some(2), Some(true)));
        let cost = v["total"]["cost_usd"].as_f64().expect("custo");
        assert!((cost - (3.0 + 0.5)).abs() < 1e-9, "{cost}");
        assert!(bad(r#"{"m":{"input_per_mtok":3,"output_per_mtok":15,"image_per_unit":-1}}"#).await.is_err());
        bad(r#"{"m":{"input_per_mtok":3,"output_per_mtok":15,"image_per_unit":0.1}}"#).await.expect("ok");
        let v = body(get(Some("s1")).await.expect("ok")).await;
        assert!(v["total"]["images_estimated"].is_null(), "preço do usuário não é estimado");
        assert!((v["total"]["images_usd"].as_f64().expect("usd") - 0.2).abs() < 1e-9);
        let v = body(get(None).await.expect("ok")).await;
        assert!((v["total"]["cost_usd"].as_f64().expect("custo") - 3.2).abs() < 1e-9, "tokens + 2 imagens a US$ 0,10");
        assert_eq!(v["specs"][0]["tasks"][0]["id"], "T01");
        assert!(get(Some("../x")).await.is_err());
    }

    #[test]
    fn web_search_cost_uses_default_or_configured_price() {
        let r = UsageRecord {
            at: String::new(), project: String::new(), spec: None, task: None,
            agent: "a".into(), model: "m".into(), input: 0, output: 0, cached: None, reasoning: None,
            web_search_calls: Some(3),
            images: None,
            image_quality: None,
        };
        let mut pricing = price(1.0, 1.0, None);
        let c = cost_of(&r, &pricing).expect("preço");
        assert!((c - 0.03).abs() < 1e-12, "padrão 0.01 por chamada: {c}");
        if let Some(p) = pricing.get_mut("m") {
            p.web_search_per_call = Some(0.025);
        }
        let c = cost_of(&r, &pricing).expect("preço");
        assert!((c - 0.075).abs() < 1e-12, "{c}");
    }

    #[test]
    fn web_search_calls_are_recorded_and_count_in_budget() {
        let root = tmp("websearch");
        let mut e = ev(Some("s1"), Some("T01"), "architect", "m", Usage::new(1_000_000, 0));
        e.web_search_calls = 2;
        FileUsageSink::new(&root, "p").record(e);
        let recs = load_records(&root, Some("s1"));
        assert_eq!(recs[0].web_search_calls, Some(2));
        // 1M tokens a US$ 1 + 2 chamadas a US$ 0,01 padrão = 1,02
        let view = aggregate(&recs, &price(1.0, 1.0, None));
        let cost = view.total.cost_usd.expect("custo");
        assert!((cost - 1.02).abs() < 1e-9, "{cost}");
        fs::write(root.join(".sdd/specs/s1/budget.json"), r#"{"max_usd":1.01}"#).expect("budget");
        assert!(budget_status(&root, "s1", &price(1.0, 1.0, None)).is_some());
        fs::write(root.join(".sdd/specs/s1/budget.json"), r#"{"max_usd":1.03}"#).expect("budget");
        assert!(budget_status(&root, "s1", &price(1.0, 1.0, None)).is_none());
    }

    #[test]
    fn images_are_recorded_and_old_lines_read_as_zero() {
        let root = tmp("images");
        let mut e = ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(10, 5));
        e.images = 1;
        FileUsageSink::new(&root, "p").record(e);
        FileUsageSink::new(&root, "p").record(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1, 1)));
        let raw = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("ler");
        let lines: Vec<&str> = raw.lines().collect();
        assert!(lines[0].contains(r#""images":1"#), "{raw}");
        assert!(!lines[1].contains("images"), "ausente quando zero: {raw}");
        let recs = load_records(&root, Some("s1"));
        assert_eq!(recs[0].images, Some(1));
        assert_eq!(recs[1].images, None);
        // Ida e volta, e linha antiga (sem o campo) ainda lida.
        let back: UsageRecord = serde_json::from_str(lines[0]).expect("ida e volta");
        assert_eq!(back, recs[0]);
        let old = r#"{"at":"2026-01-01T00:00:00Z","project":"p","agent":"a","model":"m","input":1,"output":2}"#;
        let old: UsageRecord = serde_json::from_str(old).expect("linha antiga");
        assert_eq!(old.images, None);
    }

    fn rec_at(task: &str, secs: u64) -> UsageRecord {
        UsageRecord {
            at: format_utc(secs),
            project: "p".into(),
            spec: Some("s1".into()),
            task: Some(task.into()),
            agent: "developer".into(),
            model: "m".into(),
            input: 1,
            output: 1,
            cached: None,
            reasoning: None,
            web_search_calls: None,
            images: None,
            image_quality: None,
        }
    }

    fn task<'a>(v: &'a UsageView, id: &str) -> &'a TaskTotals {
        v.specs.iter().flat_map(|s| s.tasks.iter()).find(|t| t.id == id).expect("tarefa")
    }

    fn tool_line(task: &str, ts: u64) -> String {
        format!(r#"{{"ts":{ts},"agent":"developer","task_id":"{task}","kind":"tool","text":"ler"}}"#)
    }

    #[test]
    fn parse_utc_inverts_format_utc() {
        for secs in [0, 86_399, 951_782_400, 1_760_000_000, 4_102_444_800] {
            assert_eq!(parse_utc(&format_utc(secs)), Some(secs));
        }
        assert_eq!(parse_utc("t"), None);
        assert_eq!(parse_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_utc("2026-01-01 00:00:00Z"), None);
    }

    #[test]
    fn active_secs_sums_short_gaps() {
        let t0 = 1_760_000_000;
        let v = aggregate(&[rec_at("T01", t0 + 120), rec_at("T01", t0), rec_at("T01", t0 + 300)], &BTreeMap::new());
        let t = task(&v, "T01");
        assert_eq!(t.active_secs, 300);
        assert_eq!(t.first_at.as_deref(), Some(format_utc(t0).as_str()));
        assert_eq!(t.last_at.as_deref(), Some(format_utc(t0 + 300).as_str()));
    }

    #[test]
    fn active_secs_drops_long_pause() {
        let t0 = 1_760_000_000;
        // 60 s + pausa de 11 min (descartada) + 600 s (exatamente 10 min conta).
        let recs = [rec_at("T01", t0), rec_at("T01", t0 + 60), rec_at("T01", t0 + 720), rec_at("T01", t0 + 1320)];
        let v = aggregate(&recs, &BTreeMap::new());
        let t = task(&v, "T01");
        assert_eq!(t.active_secs, 660);
        assert_eq!(t.last_at.as_deref(), Some(format_utc(t0 + 1320).as_str()));
    }

    #[test]
    fn active_secs_merges_usage_and_tools_files() {
        let root = tmp("times");
        let t0 = 1_760_000_000;
        let usage: String = [rec_at("T01", t0), rec_at("T01", t0 + 400)]
            .iter()
            .map(|r| serde_json::to_string(r).expect("json") + "\n")
            .collect();
        fs::write(root.join(".sdd/specs/s1/usage.jsonl"), usage).expect("usage");
        let trail = [tool_line("T01", t0 + 200), tool_line("T01", t0 + 500), tool_line("T02", t0 + 10), "lixo".to_string()];
        fs::write(root.join(".sdd/specs/s1/tools.jsonl"), trail.join("\n") + "\n").expect("tools");
        let recs = load_records(&root, Some("s1"));
        let marks = load_tool_marks(&root, Some("s1"));
        let v = aggregate_with_tools(&recs, &marks, &BTreeMap::new());
        // t0, +200, +400, +500 → 500 s; só com o uso seriam 400.
        assert_eq!(task(&v, "T01").active_secs, 500);
        assert_eq!(task(&v, "T01").totals.calls, 2);
        let t2 = task(&v, "T02");
        assert_eq!((t2.active_secs, t2.totals.calls), (0, 0), "tarefa só com ação aparece zerada");
        assert_eq!(t2.first_at.as_deref(), Some(format_utc(t0 + 10).as_str()));
        // Sem filtro, a trilha não é lida: o tempo por tarefa fica só com o uso.
        assert!(load_tool_marks(&root, None).is_empty());
        let all = aggregate_with_tools(&load_records(&root, None), &load_tool_marks(&root, None), &BTreeMap::new());
        assert_eq!(task(&all, "T01").active_secs, 400);
        assert!(all.specs.iter().all(|s| s.tasks.iter().all(|t| t.id != "T02")));
    }

    #[test]
    fn tool_marks_cache_rereads_only_when_trail_changes() {
        let root = tmp("times-cache");
        let path = root.join(".sdd/specs/s1/tools.jsonl");
        fs::write(&path, tool_line("T01", 10) + "\n").expect("w");
        assert_eq!(super::super::tools_trail::task_timestamps(&root, "s1"), vec![("T01".to_string(), 10)]);
        assert_eq!(super::super::tools_trail::task_timestamps(&root, "s1").len(), 1, "cache");
        fs::write(&path, tool_line("T01", 10) + "\n" + &tool_line("T02", 20) + "\n").expect("w2");
        assert_eq!(super::super::tools_trail::task_timestamps(&root, "s1").len(), 2, "arquivo mudou: relido");
        fs::remove_file(&path).expect("rm");
        assert!(super::super::tools_trail::task_timestamps(&root, "s1").is_empty());
    }

    #[test]
    fn tool_trail_symlink_is_not_followed() {
        let root = tmp("times-sym");
        let outside = tmp("times-sym-out");
        let t0 = 1_760_000_000;
        fs::write(outside.join("tools.jsonl"), tool_line("T01", t0 + 50) + "\n").expect("w");
        std::os::unix::fs::symlink(outside.join("tools.jsonl"), root.join(".sdd/specs/s1/tools.jsonl")).expect("ln");
        assert!(load_tool_marks(&root, Some("s1")).is_empty());
    }

    #[tokio::test]
    async fn usage_route_exposes_task_times() {
        use crate::config::{Config, Project};
        let root = tmp("times-api");
        let t0 = 1_760_000_000;
        fs::write(root.join(".sdd/specs/s1/usage.jsonl"), serde_json::to_string(&rec_at("T01", t0)).expect("j") + "\n").expect("w");
        fs::write(root.join(".sdd/specs/s1/tools.jsonl"), tool_line("T01", t0 + 90) + "\n").expect("w");
        let cfg = Config {
            projects: vec![Project {
                id: "p1".into(),
                name: "P".into(),
                path: root.to_string_lossy().into_owned(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            ..Config::default()
        };
        let state = AppState::new(tmp("times-api-cfg").join("config.toml"), cfg, 3000);
        let r = get_usage(State(state), AxPath("p1".into()), Query(UsageQuery { spec: Some("s1".into()) })).await.expect("ok");
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.expect("body");
        let v: serde_json::Value = serde_json::from_slice(&b).expect("json");
        let t = &v["specs"][0]["tasks"][0];
        assert_eq!(t["id"], "T01");
        assert_eq!(t["active_secs"], 90);
        assert_eq!(t["first_at"], format_utc(t0));
        assert_eq!(t["last_at"], format_utc(t0 + 90));
        assert_eq!(t["calls"], 1, "totais de antes continuam");
    }
}
