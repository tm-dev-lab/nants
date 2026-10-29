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

use axum::http::StatusCode;
use serde_json::Map;
use super::error::ApiError;
use super::events::{validate_project_id, SecretSet};
use super::projects::{find_project, format_utc, scrub_text};
use super::safe_fs::{self, SafeFsError};
use super::spec_result::validate_spec_id;
use super::state::AppState;
use crate::agents::llm_client::StreamCut;
use crate::agents::usage::{BudgetGuard, ContextEvent, ContextKind, CutEvent, UsageEvent, UsageScope, UsageSink};
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
    /// Tokens de entrada gravados no cache do provedor (já contidos em `input`; spec 132 T02).
    /// Ausente = nenhum ou não informado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    /// Chamadas de busca na web da resposta; ausente = 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_calls: Option<u64>,
    /// Imagens geradas (spec 090); ausente = 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<u64>,
    /// Qualidade das imagens geradas (`low|medium|high`, spec 092 T03); ausente = `medium`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_quality: Option<String>,
    /// Tokens estimados pelo Nants (spec 114 T05: resposta cortada sem o uso do provedor); o custo
    /// conta como estimado. Ausente = informado pelo provedor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated: Option<bool>,
    /// Motivo do corte da resposta (`too_large`, `timeout`...), quando a chamada foi cortada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cut: Option<String>,
    /// Como a chamada foi cobrada (spec 116 T02): `chatgpt_plan` pelo plano ChatGPT (tokens sem
    /// US$). Ausente = pela chave de API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<String>,
    /// Bytes do histórico enviado na chamada (spec 122 T01); ausente nos registros antigos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_bytes: Option<u64>,
    /// Mensagens do histórico enviado (spec 122 T01).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<u32>,
    /// Passo da chamada na sessão, a partir de 0 (spec 122 T01).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    /// Id estável da sessão: papel, tarefa e tentativa (spec 122 T01).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Chamada de resumo do histórico (spec 122 T04); ausente = chamada normal do laço. Entra nos
    /// totais como qualquer outra.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<bool>,
    /// Etapa da chamada (spec 128 T02: `plan_consensus`, o ciclo de consenso do plano), para medir
    /// o custo por etapa; ausente fora dela.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Rodada da etapa (no consenso, 0 = revisão inicial).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    /// Como a chamada correu (spec 133 T01); campos ausentes nos registros antigos.
    #[serde(flatten, default)]
    pub call: CallRecord,
}

/// Como a chamada correu (spec 133 T01): novas tentativas, `fallback_used`, degraus de
/// relaxamento, motivo de término e o esforço efetivo. Tudo opcional e omitido quando vazio.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRecord {
    /// Novas tentativas do mesmo pedido; ausente = nenhuma.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u32>,
    /// O pedido só passou sem o raciocínio pedido; ausente = não.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_used: Option<bool>,
    /// Degraus de relaxamento aplicados, em ordem; ausente = nenhum.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relaxed: Vec<String>,
    /// Motivo de término (`stop`, `length`, `tool_calls`, `cut` ou o do provedor saneado).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    /// Esforço efetivo (`low|medium|high`, ou `off` quando a escada o retirou).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl CallRecord {
    /// Registro do rastro da chamada, só com rótulos seguros.
    fn of(meta: crate::agents::usage::CallMeta) -> Self {
        Self {
            retries: (meta.retries > 0).then_some(meta.retries),
            fallback_used: meta.fallback_used.then_some(true),
            relaxed: meta.relaxed.iter().filter_map(|s| safe_label(s)).take(16).collect(),
            finish: meta.finish.and_then(|f| safe_label(&f)),
            effort: meta.effort.and_then(|e| safe_label(&e)),
        }
    }
}

/// Valor de `billing` das chamadas pelo plano ChatGPT.
pub const BILLING_CHATGPT_PLAN: &str = "chatgpt_plan";

impl UsageRecord {
    /// Chamada pelo plano ChatGPT: entra só no bloco [`PlanTotals`], sem US$.
    pub fn is_chatgpt_plan(&self) -> bool {
        self.billing.as_deref() == Some(BILLING_CHATGPT_PLAN)
    }
}

/// Limite de tamanho de cada `cuts.jsonl` (1 MiB): diagnósticos de respostas cortadas.
pub const MAX_CUTS_BYTES: u64 = 1024 * 1024;
const CUTS_FILE: &str = "cuts.jsonl";
/// Arquivo dos diagnósticos sem spec (chat).
const NO_SPEC_CUTS_FILE: &str = ".sdd/specs/.cuts.jsonl";
/// Linhas do exclude local para os diagnósticos de corte (por spec e do chat).
pub const CUTS_EXCLUDE_LINE: &str = ".sdd/specs/**/cuts.jsonl";
pub const CUTS_CHAT_EXCLUDE_LINE: &str = ".sdd/specs/.cuts.jsonl";

/// Limite de tamanho de cada `context.jsonl` (1 MiB): eventos de contexto (spec 122 T01).
pub const MAX_CONTEXT_BYTES: u64 = 1024 * 1024;
const CONTEXT_FILE: &str = "context.jsonl";
/// Arquivo dos eventos de contexto sem spec (chat).
const NO_SPEC_CONTEXT_FILE: &str = ".sdd/specs/.context.jsonl";
/// Linhas do exclude local para os eventos de contexto (por spec e do chat).
pub const CONTEXT_EXCLUDE_LINE: &str = ".sdd/specs/**/context.jsonl";
pub const CONTEXT_CHAT_EXCLUDE_LINE: &str = ".sdd/specs/.context.jsonl";

/// Sink que anexa ao `usage.jsonl` do projeto. Falhas de I/O são ignoradas (não derrubam o agente).
/// Os diagnósticos de resposta cortada vão para `cuts.jsonl`, com a redação de segredos.
pub struct FileUsageSink {
    root: PathBuf,
    project: String,
    secrets: Arc<SecretSet>,
    /// Etapa e rodada gravadas em cada linha (spec 128 T02); `None` fora de uma etapa.
    stage: Option<(String, u32)>,
}

impl FileUsageSink {
    /// Sem segredos da configuração: os trechos de corte só passam pela redação de URLs (e a
    /// chave da conexão já sai redigida do cliente). Produção usa [`Self::redacting`].
    pub fn new(root: impl Into<PathBuf>, project: impl Into<String>) -> Arc<Self> {
        Self::redacting(root, project, Arc::new(SecretSet::new(Vec::new())))
    }

    /// Como [`Self::new`], com os segredos da configuração para redigir os trechos de corte.
    pub fn redacting(root: impl Into<PathBuf>, project: impl Into<String>, secrets: Arc<SecretSet>) -> Arc<Self> {
        Arc::new(Self { root: root.into(), project: project.into(), secrets, stage: None })
    }

    /// [`Self::redacting`] com a etapa e a rodada gravadas em cada linha do uso (spec 128 T02).
    pub fn redacting_staged(
        root: impl Into<PathBuf>,
        project: impl Into<String>,
        secrets: Arc<SecretSet>,
        stage: &str,
        round: u32,
    ) -> Arc<Self> {
        Arc::new(Self { root: root.into(), project: project.into(), secrets, stage: Some((stage.to_string(), round)) })
    }

    /// Escopo de registro para um agente do projeto.
    pub fn scope(
        root: &Path,
        project: &str,
        agent: &str,
        spec: Option<String>,
        task: Option<String>,
    ) -> UsageScope {
        UsageScope::new(Self::new(root, project), agent, spec, task)
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
            cache_write: event.usage.cache_write.filter(|n| *n > 0).map(u64::from),
            web_search_calls: (event.web_search_calls > 0).then_some(u64::from(event.web_search_calls)),
            images: (event.images > 0).then_some(u64::from(event.images)),
            image_quality: event
                .image_quality
                .filter(|q| event.images > 0 && IMAGE_QUALITIES.contains(&q.as_str())),
            estimated: event.estimated.then_some(true),
            cut: event.cut.filter(|c| valid_cut_reason(c)),
            billing: event.billing.as_record().map(str::to_string),
            context_bytes: event.context.as_ref().map(|c| c.bytes),
            messages: event.context.as_ref().map(|c| c.messages),
            step: event.context.as_ref().map(|c| c.step),
            summary: event.context.as_ref().and_then(|c| c.summary.then_some(true)),
            session: event.context.and_then(|c| safe_label(&c.session)),
            stage: self.stage.as_ref().map(|(stage, _)| stage.clone()),
            round: self.stage.as_ref().map(|(_, round)| *round),
            call: CallRecord::of(event.call),
        };
        if let Ok(line) = serde_json::to_string(&record) {
            let _ = safe_fs::append_bounded(&self.root, &rel, &line, MAX_USAGE_BYTES);
        }
    }

    fn record_context(&self, event: ContextEvent) {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let spec = event.spec.filter(|s| validate_spec_id(s).is_ok());
        let rel = match &spec {
            Some(s) => format!("{SPECS_DIR}/{s}/{CONTEXT_FILE}"),
            None => NO_SPEC_CONTEXT_FILE.to_string(),
        };
        let tools: BTreeMap<String, u32> =
            event.tools.into_iter().filter_map(|(name, n)| safe_label(&name).map(|name| (name, n))).collect();
        let line = ContextLine {
            at: format_utc(secs),
            kind: event.kind.as_str().to_string(),
            spec,
            task: event.task.and_then(|t| safe_label(&t)),
            agent: safe_label(&event.agent).unwrap_or_default(),
            session: safe_label(&event.session).unwrap_or_default(),
            step: event.step,
            before_bytes: event.before_bytes,
            after_bytes: event.after_bytes,
            items: event.items,
            tools,
            input_tokens: event.input_tokens,
            output_tokens: event.output_tokens,
            reason: event.reason.filter(|r| valid_cut_reason(r)).map(str::to_string),
        };
        if let Ok(line) = serde_json::to_string(&line) {
            let _ = safe_fs::append_bounded(&self.root, &rel, &line, MAX_CONTEXT_BYTES);
        }
    }

    fn record_cut(&self, event: CutEvent) {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let spec = event.spec.filter(|s| validate_spec_id(s).is_ok());
        let rel = match &spec {
            Some(s) => format!("{SPECS_DIR}/{s}/{CUTS_FILE}"),
            None => NO_SPEC_CUTS_FILE.to_string(),
        };
        let line = cut_line(&format_utc(secs), &self.project, spec, event.task, &event.agent, &event.model, &event.cut, &self.secrets);
        if let Ok(line) = serde_json::to_string(&line) {
            let _ = safe_fs::append_bounded(&self.root, &rel, &line, MAX_CUTS_BYTES);
        }
    }
}

/// Rótulo gravado nos registros de contexto (sessão, tarefa, papel, ferramenta): só
/// `[A-Za-z0-9_.:-]`, até 80 bytes; outro texto vira `None` (nada de conteúdo do usuário).
fn safe_label(s: &str) -> Option<String> {
    let ok = !s.is_empty()
        && s.len() <= 80
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
    ok.then(|| s.to_string())
}

/// Linha de `context.jsonl` (spec 122 T01): um evento de contexto, só números e nomes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextLine {
    pub at: String,
    /// `compact`, `spill`, `dedup`, `summary`, `summary_failed`, `evidence_check` (spec 133 T06),
    /// `compact_refused` (spec 133 T13) ou `kb` (spec 134 T04: uma chamada de `kb_*`, com os bytes
    /// do resultado em `after_bytes`).
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub step: u32,
    #[serde(default)]
    pub before_bytes: u64,
    #[serde(default)]
    pub after_bytes: u64,
    #[serde(default)]
    pub items: u32,
    /// Itens afetados por ferramenta (só nomes).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, u32>,
    /// Tokens de entrada da chamada de resumo (spec 122 T04).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Tokens de saída da chamada de resumo (spec 122 T04).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// `summary`: o gatilho (`threshold` ou `overflow`); `summary_failed`: o motivo (`empty`,
    /// `truncated`, `larger`, `provider`). Vocabulário fechado `[a-z_]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Motivo de corte aceito no registro: o vocabulário fixo `[a-z_]`, curto.
fn valid_cut_reason(c: &str) -> bool {
    !c.is_empty() && c.len() <= 40 && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Linha de `cuts.jsonl` (spec 114 T05): números do stream, tokens e os trechos do início e do fim
/// do texto gerado, redigidos (segredos da configuração e credenciais em URL) e limitados a
/// `CUT_SNIPPET_BYTES` cada. Nunca a resposta inteira.
#[allow(clippy::too_many_arguments)]
fn cut_line(
    at: &str,
    project: &str,
    spec: Option<String>,
    task: Option<String>,
    agent: &str,
    model: &str,
    cut: &StreamCut,
    secrets: &SecretSet,
) -> serde_json::Value {
    let (head, tail) = cut.snippets(|t| scrub_text(secrets, t));
    let (usage, estimated) = match cut.usage() {
        Some((u, e)) => (Some(u), e),
        None => (None, false),
    };
    serde_json::json!({
        "at": at,
        "project": project,
        "spec": spec,
        "task": task.map(|t| scrub_text(secrets, &t)),
        "agent": scrub_text(secrets, agent),
        "model": scrub_text(secrets, model),
        "reason": cut.reason,
        "elapsed_ms": cut.elapsed_ms,
        "bytes": cut.bytes,
        "events": cut.events,
        "top_events": cut.top_events.iter().map(|(k, n)| serde_json::json!({ "type": k, "count": n })).collect::<Vec<_>>(),
        "generated_chars": cut.generated_chars,
        "input_tokens": usage.map(|u| u.prompt),
        "output_tokens": usage.map(|u| u.completion),
        "reasoning_tokens": usage.and_then(|u| u.reasoning),
        "tokens_estimated": estimated,
        "head": head,
        "tail": tail,
    })
}

/// Totais de um grupo de chamadas.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Totals {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub reasoning: u64,
    /// Tokens de entrada gravados no cache do provedor (spec 132 T10; contidos em `input`, omitido
    /// quando zero).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cache_write: u64,
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
    /// Chamadas pelo plano ChatGPT (spec 116 T02): só tokens, sem US$. Ficam fora das contagens
    /// acima (chamadas, tokens, custo e chamadas sem preço) e do orçamento em dólar; omitido sem
    /// nenhuma.
    #[serde(default, skip_serializing_if = "PlanTotals::is_empty")]
    pub chatgpt_plan: PlanTotals,
}

/// Totais das chamadas pelo plano ChatGPT (spec 116 T02): tokens, sem US$.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PlanTotals {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub reasoning: u64,
    /// Chamadas de busca na web somadas (omitido quando zero).
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub web_search_calls: u64,
}

impl PlanTotals {
    fn is_empty(&self) -> bool {
        self.calls == 0
    }

    fn add(&mut self, r: &UsageRecord) {
        self.calls += 1;
        self.input += r.input;
        self.output += r.output;
        self.cached += r.cached.unwrap_or(0);
        self.reasoning += r.reasoning.unwrap_or(0);
        self.web_search_calls += r.web_search_calls.unwrap_or(0);
    }
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

impl Totals {
    fn add(&mut self, r: &UsageRecord, pricing: &BTreeMap<String, Pricing>) {
        if r.is_chatgpt_plan() {
            self.chatgpt_plan.add(r);
            return;
        }
        self.calls += 1;
        self.input += r.input;
        self.output += r.output;
        self.cached += r.cached.unwrap_or(0);
        self.reasoning += r.reasoning.unwrap_or(0);
        self.cache_write += r.cache_write.unwrap_or(0);
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
                    || r.estimated == Some(true)
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
/// senão, o preço de entrada. A entrada gravada no cache (`cache_write`, spec 132 T02) usa
/// `cache_write_per_mtok` quando houver; senão, também o preço de entrada. As duas partes estão
/// contidas em `input` e nunca passam dele. As imagens ([`image_cost`]) somam ao custo dos tokens. Pelo plano
/// ChatGPT (spec 116 T02), sem US$: `None`.
pub fn cost_of(r: &UsageRecord, pricing: &BTreeMap<String, Pricing>) -> Option<f64> {
    if r.is_chatgpt_plan() {
        return None;
    }
    let images = image_cost(r, pricing).map(|(usd, _)| usd);
    let Some(p) = pricing.get(&r.model) else {
        return images;
    };
    let cached = r.cached.unwrap_or(0).min(r.input);
    let written = r.cache_write.unwrap_or(0).min(r.input - cached);
    let cached_price = p.cached_input_per_mtok.unwrap_or(p.input_per_mtok);
    let write_price = p.cache_write_per_mtok.unwrap_or(p.input_per_mtok);
    let total = (r.input - cached - written) as f64 * p.input_per_mtok
        + cached as f64 * cached_price
        + written as f64 * write_price
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
    /// Resumo do contexto da spec (spec 122 T01).
    pub context: ContextSummary,
}

/// Resumo do contexto (spec 122 T01), o instrumento de calibração da gestão de contexto. Conta
/// todas as chamadas, inclusive as do plano ChatGPT: o assunto são tokens, não US$.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ContextSummary {
    pub calls: u64,
    /// Tokens de entrada somados.
    pub input: u64,
    /// Entrada média por chamada (tokens, arredondada para baixo).
    pub avg_input: u64,
    /// Maior entrada de uma chamada (tokens).
    pub max_input: u64,
    /// Maior contexto medido em bytes (`context_bytes`); 0 sem chamada medida.
    pub max_context_bytes: u64,
    /// Tokens de entrada lidos do cache do provedor (`cached`), somados (spec 123 T03).
    pub cached: u64,
    /// Percentual da entrada lida do cache (`cached` / `input`, arredondado); 0 sem entrada.
    pub cache_pct: u64,
    /// Compactações registradas no `context.jsonl`.
    pub compactions: u64,
    /// Saídas grandes guardadas em arquivo (eventos `spill`, spec 123 T03).
    pub spills: u64,
    /// Releituras de arquivo inalterado evitadas (eventos `dedup`, spec 123 T03).
    pub dedups: u64,
    /// Resumos do histórico bem-sucedidos (`summary`, spec 122 T04); as falhas não contam.
    pub summaries: u64,
    /// Bytes evitados: soma de `before_bytes - after_bytes` de todos os eventos de contexto.
    pub saved_bytes: u64,
    /// Tokens que deixaram de ser reenviados, estimados (spec 123 T03, ver
    /// [`reinjection_avoided_tokens`]): o que cada evento tirou do histórico vezes as chamadas que
    /// a mesma sessão fez depois dele, dividido por 4.
    pub reinjection_avoided_tokens: u64,
}

impl ContextSummary {
    fn add_record(&mut self, r: &UsageRecord) {
        self.calls += 1;
        self.input += r.input;
        self.avg_input = self.input / self.calls;
        self.max_input = self.max_input.max(r.input);
        self.max_context_bytes = self.max_context_bytes.max(r.context_bytes.unwrap_or(0));
        self.cached += r.cached.unwrap_or(0).min(r.input);
        self.cache_pct = percent(self.cached, self.input);
    }

    fn add_event(&mut self, e: &ContextLine) {
        if e.kind == ContextKind::Compact.as_str() {
            self.compactions += 1;
        } else if e.kind == ContextKind::Spill.as_str() {
            self.spills += 1;
        } else if e.kind == ContextKind::Dedup.as_str() {
            self.dedups += 1;
        } else if e.kind == ContextKind::Summary.as_str() {
            self.summaries += 1;
        }
        self.saved_bytes += e.before_bytes.saturating_sub(e.after_bytes);
    }
}

/// `part` em % de `whole`, arredondado; 0 sem `whole`.
fn percent(part: u64, whole: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    let pct = (u128::from(part) * 100 + u128::from(whole) / 2) / u128::from(whole);
    u64::try_from(pct).unwrap_or(u64::MAX)
}

/// Bytes por token da estimativa (o mesmo divisor do `Compactor` e da linha dos Custos).
const BYTES_PER_TOKEN: u64 = 4;

/// Um histórico de uma sessão: as chamadas em ordem, com o passo e o instante (segundos Unix).
struct History {
    calls: Vec<(u32, u64)>,
}

/// Tokens que deixaram de ser reenviados, por spec (spec 123 T03). Uma estimativa, não uma
/// medida:
///
/// - cada evento de contexto tirou `before_bytes - after_bytes` do histórico da sessão, a partir
///   do passo do evento (o passo é o da chamada que já sai reduzida);
/// - esses bytes deixam de ir em cada chamada seguinte do **mesmo histórico**: as chamadas da
///   mesma `session` (na mesma spec) com `step` maior ou igual ao do evento;
/// - **um `session` junta vários históricos** quando o laço recomeça (o `implement` de novo depois
///   de um teste falho): o `step` volta a 0. Por isso as chamadas da sessão são cortadas em
///   históricos sempre que o `step` não cresce, e só contam as do histórico do evento. O evento
///   pertence ao histórico mais recente cuja chamada do passo anterior (`step - 1`) já tinha
///   acontecido no instante do evento; no passo 0, ao primeiro histórico que começa depois dele;
/// - um `summary` refaz o histórico inteiro: o que um evento anterior tirou deixa de contar a
///   partir do passo do resumo (sem o evento, o resumo teria descartado o mesmo trecho), e o
///   próprio resumo conta o que tirou dali em diante;
/// - `summary_failed` não tira nada; registros sem `session` ou `step` (antigos) não entram, e o
///   evento sem chamada medida depois dele conta 0.
///
/// O resultado em bytes é dividido por [`BYTES_PER_TOKEN`].
pub fn reinjection_avoided_tokens(records: &[UsageRecord], events: &[ContextLine]) -> BTreeMap<Option<String>, u64> {
    type Key = (Option<String>, String);
    let mut histories: BTreeMap<Key, Vec<History>> = BTreeMap::new();
    for r in records {
        let (Some(session), Some(step)) = (&r.session, r.step) else {
            continue;
        };
        let at = parse_utc(&r.at).unwrap_or(0);
        let list = histories.entry((r.spec.clone(), session.clone())).or_default();
        match list.last_mut() {
            Some(h) if h.calls.last().is_some_and(|&(prev, _)| step > prev) => h.calls.push((step, at)),
            _ => list.push(History { calls: vec![(step, at)] }),
        }
    }
    // Eventos de cada histórico: (passo, bytes tirados, é resumo).
    let mut per_history: BTreeMap<(Key, usize), Vec<(u32, u64, bool)>> = BTreeMap::new();
    for e in events {
        // Spec 133 T06: a conferência da evidência não mexe no histórico.
        // Spec 133 T13: a compactação recusada também não.
        if e.kind == ContextKind::SummaryFailed.as_str()
            || e.kind == ContextKind::EvidenceCheck.as_str()
            || e.kind == ContextKind::CompactRefused.as_str()
        {
            continue;
        }
        let key = (e.spec.clone(), e.session.clone());
        let Some(list) = histories.get(&key) else {
            continue;
        };
        let at = parse_utc(&e.at).unwrap_or(0);
        let index = if e.step == 0 {
            list.iter().position(|h| h.calls.first().is_some_and(|&(_, t)| t >= at))
        } else {
            list.iter().rposition(|h| h.calls.iter().any(|&(s, t)| s == e.step - 1 && t <= at))
        };
        let Some(index) = index else {
            continue;
        };
        let is_summary = e.kind == ContextKind::Summary.as_str();
        per_history
            .entry((key, index))
            .or_default()
            .push((e.step, e.before_bytes.saturating_sub(e.after_bytes), is_summary));
    }
    let mut out: BTreeMap<Option<String>, u64> = BTreeMap::new();
    for ((key, index), evs) in per_history {
        let Some(history) = histories.get(&key).and_then(|l| l.get(index)) else {
            continue;
        };
        let mut bytes: u64 = 0;
        for &(step, removed, _) in &evs {
            // O próximo resumo depois do evento encerra o efeito dele.
            let until = evs.iter().filter(|&&(s, _, summary)| summary && s > step).map(|&(s, _, _)| s).min();
            let later = history.calls.iter().filter(|&&(s, _)| s >= step && until.is_none_or(|u| s < u)).count();
            bytes = bytes.saturating_add(removed.saturating_mul(u64::try_from(later).unwrap_or(u64::MAX)));
        }
        if bytes >= BYTES_PER_TOKEN {
            let tokens = out.entry(key.0).or_default();
            *tokens = tokens.saturating_add(bytes / BYTES_PER_TOKEN);
        }
    }
    out
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
    /// Resumo do contexto do projeto ou da spec consultada (spec 122 T01).
    pub context: ContextSummary,
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
    let mut context = ContextSummary::default();
    let mut spec_context: BTreeMap<Option<String>, ContextSummary> = BTreeMap::new();
    for r in records {
        context.add_record(r);
        spec_context.entry(r.spec.clone()).or_default().add_record(r);
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
                let context = spec_context.remove(&spec).unwrap_or_default();
                SpecUsage { spec, totals, tasks, agents: named(agents), context }
            })
            .collect(),
        total,
        by_model: named(models),
        by_agent: named(agents),
        by_spec: named(specs),
        budget: None,
        context,
    }
}

/// Soma os eventos de contexto (`context.jsonl`) ao resumo do projeto e ao de cada spec, com os
/// tokens que deixaram de ser reenviados (estimativa, [`reinjection_avoided_tokens`]), calculados
/// com as chamadas `records` da mesma consulta.
pub fn add_context_events(view: &mut UsageView, events: &[ContextLine], records: &[UsageRecord]) {
    for e in events {
        view.context.add_event(e);
        if let Some(s) = view.specs.iter_mut().find(|s| s.spec == e.spec) {
            s.context.add_event(e);
        }
    }
    for (spec, tokens) in reinjection_avoided_tokens(records, events) {
        view.context.reinjection_avoided_tokens = view.context.reinjection_avoided_tokens.saturating_add(tokens);
        if let Some(s) = view.specs.iter_mut().find(|s| s.spec == spec) {
            s.context.reinjection_avoided_tokens = s.context.reinjection_avoided_tokens.saturating_add(tokens);
        }
    }
}

/// Linhas válidas de um arquivo; ausente ou ilegível → vazio. Linhas inválidas são ignoradas.
fn read_lines<T: serde::de::DeserializeOwned>(root: &Path, rel: &str, max: u64) -> Vec<T> {
    match safe_fs::read_regular(root, rel, max + 4096) {
        Ok(text) => text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect(),
        Err(SafeFsError::Missing) | Err(_) => Vec::new(),
    }
}

/// Linhas de um arquivo da spec (ou, sem filtro, de todas e do arquivo sem spec).
fn load_spec_files<T: serde::de::DeserializeOwned>(
    root: &Path,
    spec: Option<&str>,
    file: &str,
    no_spec_file: &str,
    max: u64,
) -> Vec<T> {
    if let Some(s) = spec {
        return read_lines(root, &format!("{SPECS_DIR}/{s}/{file}"), max);
    }
    let mut out = read_lines(root, no_spec_file, max);
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
        out.extend(read_lines(root, &format!("{SPECS_DIR}/{n}/{file}"), max));
    }
    out
}

/// Lê o uso de uma spec ou, sem filtro, de todas (inclusive o arquivo sem spec).
pub fn load_records(root: &Path, spec: Option<&str>) -> Vec<UsageRecord> {
    load_spec_files(root, spec, USAGE_FILE, NO_SPEC_FILE, MAX_USAGE_BYTES)
}

/// Eventos de contexto (`context.jsonl`, spec 122 T01) de uma spec ou, sem filtro, de todas. Cada
/// linha leva a própria spec (a do chat fica `None`).
pub fn load_context_events(root: &Path, spec: Option<&str>) -> Vec<ContextLine> {
    load_spec_files(root, spec, CONTEXT_FILE, NO_SPEC_CONTEXT_FILE, MAX_CONTEXT_BYTES)
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
pub fn budget_status(root: &Path, spec: &str, pricing: &BTreeMap<String, Pricing>, lang: crate::i18n::Lang) -> Option<String> {
    let budget = read_budget(root, spec)?;
    let view = aggregate(&load_records(root, Some(spec)), pricing);
    // O limite de tokens conta também os do plano ChatGPT; o de US$ não (spec 116 T02).
    let plan = &view.total.chatgpt_plan;
    let tokens = view.total.input + view.total.output + plan.input + plan.output;
    if let Some(max) = budget.max_tokens {
        if tokens >= max {
            let (tokens, max) = (tokens.to_string(), max.to_string());
            return Some(crate::i18n::tr(lang, "runner.budget.tokens", &[("tokens", &tokens), ("max", &max)]));
        }
    }
    if let (Some(max), Some(cost)) = (budget.max_usd, view.total.cost_usd) {
        if cost >= max {
            let (cost, max) = (format!("{cost:.2}"), format!("{max:.2}"));
            return Some(crate::i18n::tr(lang, "runner.budget.usd", &[("cost", &cost), ("max", &max)]));
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
    fn exceeded(&self, spec: &str, lang: crate::i18n::Lang) -> Option<String> {
        budget_status(&self.root, spec, &self.pricing, lang)
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
        .map_err(|_| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "json_invalid_fields", Map::new()))?;
    if !budget.is_valid() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "budget_invalid", Map::new()));
    }
    let project = find_project(&state, &id).await?;
    let root = PathBuf::from(&project.path);
    tokio::task::spawn_blocking(move || {
        if budget.is_empty() {
            return safe_fs::remove_regular(&root, &budget_rel(&spec))
                .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "budget_remove_failed", Map::new()));
        }
        if !root.join(SPECS_DIR).join(&spec).is_dir() {
            return Err(ApiError::coded(axum::http::StatusCode::NOT_FOUND, "spec_not_found", Map::new()));
        }
        write_budget(&root, &spec, &budget).map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "budget_write_failed", Map::new()))
    })
    .await
    .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))??;
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
        add_context_events(&mut view, &load_context_events(&root, q.spec.as_deref()), &records);
        if q.spec.is_some() {
            view.by_spec.clear();
        }
        view.budget = q.spec.as_deref().and_then(|s| read_budget(&root, s));
        view
    })
    .await
    .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?;
    Ok(Json(view).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::llm_client::Usage;
    use crate::agents::usage::ContextStats;
    use crate::i18n::Lang;
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
            estimated: false,
            cut: None,
            billing: Default::default(),
            context: None,
            call: Default::default(),
        }
    }

    fn price(i: f64, o: f64, c: Option<f64>) -> BTreeMap<String, Pricing> {
        BTreeMap::from([("m".to_string(), Pricing { input_per_mtok: i, output_per_mtok: o, cached_input_per_mtok: c, cache_write_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None })])
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
            cache_write: None,
            web_search_calls: w,
            images: None,
            image_quality: None,
            estimated: None,
            cut: None,
            billing: None,
            context_bytes: None,
            messages: None,
            step: None,
            session: None,
            summary: None,
            stage: None,
            round: None, call: Default::default(),
        };
        let pr = BTreeMap::from([("m".to_string(), Pricing { input_per_mtok: 0.0, output_per_mtok: 0.0, cached_input_per_mtok: None, cache_write_per_mtok: None, web_search_per_call: Some(0.5), image_per_unit: None, source: Default::default(), estimated_at: None })]);
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
        sink.record(ev(Some("s1"), Some("T01"), "developer", "m", Usage { prompt: 1000, completion: 500, cached: Some(400), reasoning: Some(100), cache_write: None }));
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
            ("m".to_string(), Pricing { input_per_mtok: 2.0, output_per_mtok: 8.0, cached_input_per_mtok: None, cache_write_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None }),
            ("other".to_string(), Pricing { input_per_mtok: 100.0, output_per_mtok: 200.0, cached_input_per_mtok: None, cache_write_per_mtok: None, web_search_per_call: None, image_per_unit: None, source: Default::default(), estimated_at: None }),
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
            agent: "a".into(), model: "m".into(), input: 1000, output: 0, cached: Some(400), reasoning: None, cache_write: None, web_search_calls: None, images: None, image_quality: None, estimated: None, cut: None, billing: None, context_bytes: None, messages: None, step: None, session: None, summary: None, stage: None, round: None, call: Default::default(),
        };
        let c = cost_of(&r, &price(1.0, 1.0, None)).expect("preço");
        assert!((c - 0.001).abs() < 1e-12);
        assert_eq!(cost_of(&r, &BTreeMap::new()), None);
    }

    /// Spec 132 T02: a escrita de cache (contida em `input`) usa `cache_write_per_mtok`; sem esse
    /// preço, cobra como entrada. Nunca passa da entrada que sobra fora da leitura de cache.
    #[test]
    fn cache_write_is_billed_with_its_own_price() {
        let rec = |input: u64, cached: Option<u64>, write: Option<u64>| UsageRecord {
            at: String::new(), project: String::new(), spec: None, task: None,
            agent: "a".into(), model: "m".into(), input, output: 100, cached, reasoning: None, cache_write: write, web_search_calls: None, images: None, image_quality: None, estimated: None, cut: None, billing: None, context_bytes: None, messages: None, step: None, session: None, summary: None, stage: None, round: None, call: Default::default(),
        };
        let mut pr = price(3.0, 15.0, Some(0.3));
        if let Some(p) = pr.get_mut("m") {
            p.cache_write_per_mtok = Some(3.75);
        }
        // 1000 de entrada: 600 lidos do cache, 300 gravados, 100 normais; 100 de saída.
        let c = cost_of(&rec(1000, Some(600), Some(300)), &pr).expect("preço");
        let want = (100.0 * 3.0 + 600.0 * 0.3 + 300.0 * 3.75 + 100.0 * 15.0) / 1e6;
        assert!((c - want).abs() < 1e-12, "{c} != {want}");
        // Sem preço de escrita: a escrita cobra como entrada normal.
        let plain = cost_of(&rec(1000, Some(600), Some(300)), &price(3.0, 15.0, Some(0.3))).expect("preço");
        assert!((plain - (400.0 * 3.0 + 600.0 * 0.3 + 100.0 * 15.0) / 1e6).abs() < 1e-12, "{plain}");
        // Escrita maior que a entrada que sobra: limitada a ela.
        let capped = cost_of(&rec(1000, Some(600), Some(5000)), &pr).expect("preço");
        assert!((capped - (600.0 * 0.3 + 400.0 * 3.75 + 100.0 * 15.0) / 1e6).abs() < 1e-12, "{capped}");
        // Sem escrita: o custo de antes.
        let none = cost_of(&rec(1000, Some(600), None), &pr).expect("preço");
        assert!((none - (400.0 * 3.0 + 600.0 * 0.3 + 100.0 * 15.0) / 1e6).abs() < 1e-12, "{none}");
    }

    /// Spec 132 T10: a escrita de cache entra nos totais (para a tela de Custos) e só aparece no
    /// JSON quando houver.
    #[test]
    fn totals_sum_cache_write() {
        let rec = |write: Option<u64>| UsageRecord {
            at: String::new(), project: String::new(), spec: None, task: None,
            agent: "a".into(), model: "m".into(), input: 1000, output: 10, cached: Some(200), reasoning: None, cache_write: write, web_search_calls: None, images: None, image_quality: None, estimated: None, cut: None, billing: None, context_bytes: None, messages: None, step: None, session: None, summary: None, stage: None, round: None, call: Default::default(),
        };
        let pr = price(3.0, 15.0, None);
        let mut totals = Totals::default();
        totals.add(&rec(None), &pr);
        assert!(serde_json::to_value(&totals).expect("json").get("cache_write").is_none());
        totals.add(&rec(Some(300)), &pr);
        totals.add(&rec(Some(50)), &pr);
        assert_eq!(totals.cache_write, 350);
        assert_eq!(serde_json::to_value(&totals).expect("json")["cache_write"], 350);
    }

    /// Spec 132 T02: o registro guarda a escrita de cache do `Usage`; zero ou ausente fica fora.
    #[test]
    fn sink_records_cache_write() {
        let root = tmp("cache-write");
        let sink = FileUsageSink::new(&root, "p1");
        let with = Usage { prompt: 900, completion: 10, cached: Some(500), reasoning: None, cache_write: Some(300) };
        sink.record(ev(Some("s1"), None, "developer", "m", with));
        sink.record(ev(Some("s1"), None, "developer", "m", Usage { cache_write: Some(0), ..Usage::new(5, 1) }));
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("file");
        let recs: Vec<UsageRecord> = text.lines().map(|l| serde_json::from_str(l).expect("json")).collect();
        assert_eq!(recs[0].cache_write, Some(300));
        assert_eq!(recs[1].cache_write, None);
        assert!(!text.lines().nth(1).expect("linha").contains("cache_write"));
        let old: UsageRecord = serde_json::from_str(r#"{"at":"t","project":"p","agent":"a","model":"m","input":1,"output":1}"#).expect("antigo");
        assert_eq!(old.cache_write, None, "registro antigo carrega sem o campo");
        let _ = fs::remove_dir_all(&root);
    }

    /// Spec 092 T03: imagens × (image_per_unit ou estimativa por qualidade), somadas aos tokens.
    #[test]
    fn image_cost_uses_unit_price_or_the_default_per_quality() {
        let img = |n: u64, q: Option<&str>| UsageRecord {
            at: String::new(), project: String::new(), spec: Some("s1".into()), task: Some("T01".into()),
            agent: "developer".into(), model: "m".into(), input: 1_000_000, output: 0, cached: None, reasoning: None, cache_write: None,
            web_search_calls: None, images: Some(n), image_quality: q.map(String::from), estimated: None, cut: None, billing: None, context_bytes: None, messages: None, step: None, session: None, summary: None, stage: None, round: None, call: Default::default(),
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
        assert!(budget_status(&root, "s1", &BTreeMap::new(), Lang::PtBr).is_none(), "sem orçamento");
        write_budget(&root, "s1", &Budget { max_tokens: Some(2_000_000), max_usd: Some(2.0) }).expect("w");
        assert!(budget_status(&root, "s1", &BTreeMap::new(), Lang::PtBr).is_none(), "tokens abaixo; sem preço não há US$");
        let p = price(3.0, 0.0, None);
        let r = budget_status(&root, "s1", &p, Lang::PtBr).expect("US$ atingido");
        assert_eq!(r, "orçamento da spec atingido: US$ 3.00 de US$ 2.00");
        write_budget(&root, "s1", &Budget { max_tokens: Some(1_000_000), max_usd: None }).expect("w2");
        let r = budget_status(&root, "s1", &BTreeMap::new(), Lang::PtBr).expect("tokens atingido");
        assert_eq!(r, "orçamento da spec atingido: 1000000 de 1000000 tokens");
        assert_eq!(FileBudgetGuard::new(&root, BTreeMap::new()).exceeded("s1", Lang::PtBr), Some(r));
        // Spec 114 T02: o motivo sai no idioma do projeto.
        assert_eq!(
            FileBudgetGuard::new(&root, BTreeMap::new()).exceeded("s1", Lang::En).as_deref(),
            Some("spec budget reached: 1000000 of 1000000 tokens")
        );
        write_budget(&root, "s1", &Budget { max_tokens: None, max_usd: Some(2.0) }).expect("w3");
        assert_eq!(
            budget_status(&root, "s1", &p, Lang::En).as_deref(),
            Some("spec budget reached: US$ 3.00 of US$ 2.00")
        );
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
            agent: "a".into(), model: "m".into(), input: 0, output: 0, cached: None, reasoning: None, cache_write: None,
            web_search_calls: Some(3),
            images: None,
            image_quality: None,
            estimated: None,
            cut: None,
            billing: None,
            context_bytes: None,
            messages: None,
            step: None,
            session: None,
            summary: None,
            stage: None,
            round: None, call: Default::default(),
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
        assert!(budget_status(&root, "s1", &price(1.0, 1.0, None), Lang::PtBr).is_some());
        fs::write(root.join(".sdd/specs/s1/budget.json"), r#"{"max_usd":1.03}"#).expect("budget");
        assert!(budget_status(&root, "s1", &price(1.0, 1.0, None), Lang::PtBr).is_none());
    }

    /// Spec 116 T02: as chamadas pelo plano ChatGPT gravam `billing: chatgpt_plan`, ficam num
    /// bloco próprio (tokens, sem US$), fora do total, do custo e do orçamento em dólar; o limite
    /// de tokens ainda as conta.
    #[test]
    fn plan_calls_are_recorded_apart_without_usd() {
        use crate::agents::llm_client::Billing;
        let root = tmp("plano");
        let mut plan = ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1_000_000, 500_000));
        plan.billing = Billing::ChatgptPlan;
        plan.web_search_calls = 1;
        FileUsageSink::new(&root, "p").record(plan);
        FileUsageSink::new(&root, "p").record(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1_000_000, 0)));
        let raw = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("ler");
        let lines: Vec<&str> = raw.lines().collect();
        assert!(lines[0].contains(r#""billing":"chatgpt_plan""#), "{raw}");
        assert!(!lines[1].contains("billing"), "por chave, sem o campo: {raw}");
        let recs = load_records(&root, Some("s1"));
        assert!(recs[0].is_chatgpt_plan() && !recs[1].is_chatgpt_plan());
        assert_eq!(cost_of(&recs[0], &price(1.0, 1.0, None)), None, "sem US$");
        let view = aggregate(&recs, &price(1.0, 1.0, None));
        assert_eq!((view.total.calls, view.total.input, view.total.output), (1, 1_000_000, 0));
        assert_eq!(view.total.unpriced_calls, 0);
        let cost = view.total.cost_usd.expect("custo da chamada por chave");
        assert!((cost - 1.0).abs() < 1e-9, "o plano fica fora do US$: {cost}");
        let p = &view.total.chatgpt_plan;
        assert_eq!((p.calls, p.input, p.output, p.web_search_calls), (1, 1_000_000, 500_000, 1));
        assert_eq!(view.by_model[0].totals.chatgpt_plan.calls, 1);
        let json = serde_json::to_value(&view).expect("json");
        assert_eq!(json["total"]["chatgpt_plan"]["calls"], 1);
        assert!(json["by_agent"][0]["chatgpt_plan"].is_object());
        // Só por chave: o bloco não aparece.
        let only_key = serde_json::to_value(aggregate(&recs[1..], &price(1.0, 1.0, None))).expect("json");
        assert!(only_key["total"].get("chatgpt_plan").is_none());
        // Orçamento em US$ ignora o plano; o de tokens conta.
        fs::write(root.join(".sdd/specs/s1/budget.json"), r#"{"max_usd":1.5}"#).expect("budget");
        assert!(budget_status(&root, "s1", &price(1.0, 1.0, None), Lang::PtBr).is_none());
        fs::write(root.join(".sdd/specs/s1/budget.json"), r#"{"max_tokens":2000000}"#).expect("budget");
        assert!(budget_status(&root, "s1", &price(1.0, 1.0, None), Lang::PtBr).is_some());
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
            cache_write: None,
            web_search_calls: None,
            images: None,
            image_quality: None,
            estimated: None,
            cut: None,
            billing: None,
            context_bytes: None,
            messages: None,
            step: None,
            session: None,
            summary: None,
            stage: None,
            round: None, call: Default::default(),
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

    /// Spec 114 T05: provedor simulado manda um stream enorme e é cortado por tamanho. O
    /// diagnóstico vai para `cuts.jsonl` sem segredo e com os trechos limitados; o custo estimado
    /// entra no `usage.jsonl` com `cut: too_large` e aparece nos Custos como estimado.
    #[tokio::test]
    async fn too_large_cut_records_diagnosis_and_estimated_cost() {
        use crate::agents::chat::ChatBackend;
        use crate::agents::llm_client::stream_tests::{huge_responses_stream, raw_server};
        use crate::agents::llm_client::tests::KEY;
        use crate::agents::llm_client::{ChatMessage, ChatRequest, LlmClient, Role, CUT_SNIPPET_BYTES};
        use crate::agents::usage::UsageBackend;
        use crate::config::WireApi;
        const CONFIG_SECRET: &str = "cfg-SEGREDO-da-config-0123456789";
        let root = tmp("cut-too-large");
        let (base, _) = raw_server(vec![(200, "text/event-stream", huge_responses_stream(5_000, CONFIG_SECRET))]).await;
        let client = LlmClient::new(base, KEY.into())
            .expect("cliente")
            .with_wire_api(WireApi::Responses)
            .with_max_response_bytes(256 * 1024);
        let secrets = Arc::new(SecretSet::new(vec![CONFIG_SECRET.to_string(), KEY.to_string()]));
        let scope = UsageScope::new(FileUsageSink::redacting(&root, "p1", secrets), "developer", Some("s1".into()), Some("T01".into()));
        let metered = UsageBackend::new(&client, Some(scope));
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![ChatMessage::text(Role::User, "escreva tudo")],
            tools: Vec::new(),
            effort: None,
            response_json: false,
            web_search: false,
            cache_key: None,
            web_search_max_uses: None,
        };
        let err = metered.complete_stream(&req, &|_| {}).await.expect_err("cortada");
        assert!(format!("{err:?}").contains("TooLarge"), "{err:?}");

        let cuts = fs::read_to_string(root.join(".sdd/specs/s1/cuts.jsonl")).expect("diagnóstico");
        assert_eq!(cuts.lines().count(), 1);
        assert!(!cuts.contains(CONFIG_SECRET) && !cuts.contains(KEY), "sem segredo");
        assert!(!cuts.contains("SEGREDO"), "nem pedaço de segredo");
        assert!(cuts.len() < 8 * 1024, "nunca a resposta inteira: {} bytes", cuts.len());
        let line: serde_json::Value = serde_json::from_str(cuts.trim()).expect("json");
        assert_eq!(line["reason"], "too_large");
        assert_eq!(line["task"], "T01");
        assert_eq!(line["agent"], "developer");
        assert!(line["bytes"].as_u64().is_some_and(|b| b > 256 * 1024));
        assert!(line["events"].as_u64().is_some_and(|n| n > 100));
        assert!(line["elapsed_ms"].is_u64());
        assert_eq!(line["top_events"][0]["type"], "response.output_text.delta");
        assert_eq!(line["tokens_estimated"], true);
        let head = line["head"].as_str().expect("início");
        let tail = line["tail"].as_str().expect("fim");
        assert!(head.starts_with("INICIO ") && head.len() <= CUT_SNIPPET_BYTES, "{}", head.len());
        assert!(!tail.is_empty() && tail.len() <= CUT_SNIPPET_BYTES, "{}", tail.len());

        let records = load_records(&root, Some("s1"));
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!((r.estimated, r.cut.as_deref()), (Some(true), Some("too_large")));
        assert!(r.input > 0 && r.output >= 5_000, "{r:?}");
        let view = aggregate(&records, &price(1.0, 2.0, None));
        assert!(view.total.cost_usd.is_some_and(|c| c > 0.0));
        assert!(view.total.cost_estimated, "custo de chamada cortada aparece como estimado");
        assert_eq!(view.total.calls, 1);
    }

    /// Spec 133 T01: o rastro da chamada vai ao `usage.jsonl` só quando há o que registrar, e as
    /// linhas antigas (sem os campos) continuam carregando.
    #[test]
    fn call_trace_fields_are_recorded_and_old_lines_still_load() {
        let root = tmp("call-trace");
        FileUsageSink::new(&root, "p").record(ev(Some("s1"), None, "a", "m", Usage::new(1, 1)));
        let mut e = ev(Some("s1"), Some("T02"), "developer", "m", Usage::new(2, 1));
        e.call = crate::agents::usage::CallMeta {
            retries: 2,
            fallback_used: true,
            relaxed: vec!["effort".into(), "com espaço".into()],
            finish: Some("tool_calls".into()),
            effort: Some("off".into()),
        };
        FileUsageSink::new(&root, "p").record(e);
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("uso");
        let first = text.lines().next().expect("linha antiga");
        for key in ["retries", "fallback_used", "relaxed", "finish", "effort"] {
            assert!(!first.contains(key), "sem rastro, nenhum campo: {first}");
        }
        let records = load_records(&root, Some("s1"));
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].call, CallRecord::default());
        assert_eq!(
            records[1].call,
            CallRecord {
                retries: Some(2),
                fallback_used: Some(true),
                relaxed: vec!["effort".into()],
                finish: Some("tool_calls".into()),
                effort: Some("off".into()),
            }
        );
        let old = r#"{"at":"2026-10-01T00:00:00Z","project":"p","agent":"a","model":"m","input":1,"output":1}"#;
        let parsed: UsageRecord = serde_json::from_str(old).expect("linha antiga");
        assert_eq!(parsed.call, CallRecord::default());
    }

    #[test]
    fn usage_record_without_cut_fields_keeps_old_shape() {
        let root = tmp("cut-shape");
        FileUsageSink::new(&root, "p").record(ev(Some("s1"), None, "a", "m", Usage::new(1, 1)));
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("uso");
        assert!(!text.contains("estimated") && !text.contains("\"cut\""), "{text}");
        let mut e = ev(Some("s1"), None, "a", "m", Usage::new(1, 1));
        e.cut = Some("../segredo".into());
        FileUsageSink::new(&root, "p").record(e);
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("uso");
        assert!(!text.contains("segredo"), "motivo fora do vocabulário é descartado");
    }

    fn ctx_event(spec: Option<&str>, session: &str, before: u64, after: u64) -> ContextEvent {
        ContextEvent {
            kind: ContextKind::Compact,
            agent: "developer".into(),
            spec: spec.map(String::from),
            task: Some("T01".into()),
            session: session.into(),
            step: 7,
            before_bytes: before,
            after_bytes: after,
            items: 3,
            tools: BTreeMap::from([("read_file".to_string(), 2), ("run_command".to_string(), 1)]),
            input_tokens: None,
            output_tokens: None,
            reason: None,
        }
    }

    fn with_context(mut e: UsageEvent, bytes: u64, step: u32) -> UsageEvent {
        e.context = Some(ContextStats { bytes, messages: 4, step, session: "developer:T01:a1".into(), summary: false });
        e
    }

    /// Spec 122 T01: os campos novos de contexto são gravados e relidos.
    #[test]
    fn context_fields_are_recorded_and_read_back() {
        let root = tmp("ctx-fields");
        let sink = FileUsageSink::new(&root, "p1");
        sink.record(with_context(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(100, 1)), 2048, 3));
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("uso");
        let line: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
        assert_eq!(line["context_bytes"], 2048);
        assert_eq!(line["messages"], 4);
        assert_eq!(line["step"], 3);
        assert_eq!(line["session"], "developer:T01:a1");
        assert_eq!(line["input"], 100, "os campos antigos continuam");
        let r = &load_records(&root, Some("s1"))[0];
        assert_eq!((r.context_bytes, r.messages, r.step, r.session.as_deref()), (Some(2048), Some(4), Some(3), Some("developer:T01:a1")));
        // Sessão fora do vocabulário não é gravada (nada de texto livre).
        let mut odd = with_context(ev(Some("s1"), None, "a", "m", Usage::new(1, 1)), 1, 0);
        if let Some(c) = odd.context.as_mut() {
            c.session = "dev senha=123".into();
        }
        sink.record(odd);
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("uso");
        assert!(!text.contains("senha"), "{text}");
    }

    /// Spec 122 T01: registros antigos, sem os campos de contexto, continuam sendo lidos e somados.
    #[test]
    fn old_records_without_context_fields_are_still_read() {
        let root = tmp("ctx-old");
        let old = r#"{"at":"2026-10-23T12:49:43Z","project":"p1","spec":"s1","task":"T01","agent":"developer","model":"m","input":500,"output":10,"cached":100}"#;
        fs::write(root.join(".sdd/specs/s1/usage.jsonl"), format!("{old}\n")).expect("w");
        let records = load_records(&root, Some("s1"));
        assert_eq!(records.len(), 1);
        assert_eq!((records[0].context_bytes, records[0].step, records[0].session.as_deref()), (None, None, None));
        let mut view = aggregate(&records, &BTreeMap::new());
        add_context_events(&mut view, &load_context_events(&root, Some("s1")), &records);
        assert_eq!(view.total.calls, 1);
        assert_eq!(
            view.context,
            ContextSummary {
                calls: 1,
                input: 500,
                avg_input: 500,
                max_input: 500,
                max_context_bytes: 0,
                cached: 100,
                cache_pct: 20,
                compactions: 0,
                spills: 0,
                dedups: 0,
                summaries: 0,
                saved_bytes: 0,
                reinjection_avoided_tokens: 0,
            }
        );
    }

    /// Spec 122 T01: o evento de compactação vai para o `context.jsonl` da spec (ou o do chat), com
    /// o antes e o depois e só números e nomes de ferramentas.
    #[test]
    fn compaction_event_is_written_with_before_and_after() {
        let root = tmp("ctx-events");
        let sink = FileUsageSink::new(&root, "p1");
        sink.record_context(ctx_event(Some("s1"), "developer:T01:a1", 300_000, 120_000));
        sink.record_context(ctx_event(None, "architect:-", 10, 4));
        let text = fs::read_to_string(root.join(".sdd/specs/s1/context.jsonl")).expect("eventos");
        let line: serde_json::Value = serde_json::from_str(text.trim()).expect("json");
        assert_eq!(line["kind"], "compact");
        assert_eq!(line["session"], "developer:T01:a1");
        assert_eq!(line["step"], 7);
        assert_eq!((line["before_bytes"].as_u64(), line["after_bytes"].as_u64()), (Some(300_000), Some(120_000)));
        assert_eq!(line["items"], 3);
        assert_eq!(line["tools"]["read_file"], 2);
        assert!(line["at"].as_str().is_some_and(|a| parse_utc(a).is_some()));
        assert!(root.join(".sdd/specs/.context.jsonl").is_file(), "sem spec: o arquivo do chat");
        let all = load_context_events(&root, None);
        assert_eq!(all.len(), 2);
        assert_eq!(all.iter().filter(|e| e.spec.is_none()).count(), 1);
    }

    /// Spec 122 T04: a chamada de resumo vai ao `usage.jsonl` marcada (e somada como as demais); o
    /// evento `summary` leva os tokens e o motivo, e motivo fora do vocabulário é descartado.
    #[test]
    fn summary_call_is_marked_and_summary_event_keeps_tokens_and_reason() {
        let root = tmp("ctx-summary");
        let sink = FileUsageSink::new(&root, "p1");
        let mut marked = with_context(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(900, 70)), 3600, 4);
        if let Some(c) = marked.context.as_mut() {
            c.summary = true;
        }
        sink.record(marked);
        sink.record(with_context(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(100, 5)), 400, 5));
        let text = fs::read_to_string(root.join(".sdd/specs/s1/usage.jsonl")).expect("uso");
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].contains(r#""summary":true"#), "{text}");
        assert!(!lines[1].contains("summary"), "ausente na chamada normal: {text}");
        let recs = load_records(&root, Some("s1"));
        assert_eq!((recs[0].summary, recs[1].summary), (Some(true), None));
        let view = aggregate(&recs, &BTreeMap::new());
        assert_eq!((view.total.calls, view.total.input), (2, 1000), "o resumo entra nos Custos");
        let mut e = ctx_event(Some("s1"), "developer:T01:a1", 40_000, 9_000);
        e.kind = ContextKind::Summary;
        e.input_tokens = Some(10_000);
        e.output_tokens = Some(77);
        e.reason = Some("threshold");
        sink.record_context(e.clone());
        e.kind = ContextKind::SummaryFailed;
        e.reason = Some("Fora do vocabulário");
        sink.record_context(e);
        let events = load_context_events(&root, Some("s1"));
        assert_eq!(events[0].kind, "summary");
        assert_eq!((events[0].input_tokens, events[0].output_tokens), (Some(10_000), Some(77)));
        assert_eq!(events[0].reason.as_deref(), Some("threshold"));
        assert_eq!((events[1].kind.as_str(), events[1].reason.as_deref()), ("summary_failed", None));
        let raw = fs::read_to_string(root.join(".sdd/specs/s1/context.jsonl")).expect("eventos");
        assert!(!raw.contains("Fora"), "{raw}");
    }

    /// Chamada medida da sessão `session` na spec `spec`, no passo `step`, no segundo `secs`.
    fn call(spec: &str, session: &str, step: u32, secs: u64) -> UsageRecord {
        UsageRecord {
            at: format_utc(1_800_000_000 + secs),
            project: "p".into(),
            spec: Some(spec.into()),
            task: Some("T01".into()),
            agent: "developer".into(),
            model: "m".into(),
            input: 1000,
            output: 10,
            cached: None,
            reasoning: None,
            cache_write: None,
            web_search_calls: None,
            images: None,
            image_quality: None,
            estimated: None,
            cut: None,
            billing: None,
            context_bytes: Some(4000),
            messages: Some(3),
            step: Some(step),
            session: Some(session.into()),
            summary: None,
            stage: None,
            round: None, call: Default::default(),
        }
    }

    /// Evento `kind` da sessão no passo `step`, no segundo `secs`, que tirou `removed` bytes.
    fn event(spec: &str, session: &str, kind: &str, step: u32, secs: u64, removed: u64) -> ContextLine {
        ContextLine {
            at: format_utc(1_800_000_000 + secs),
            kind: kind.into(),
            spec: Some(spec.into()),
            task: Some("T01".into()),
            agent: "developer".into(),
            session: session.into(),
            step,
            before_bytes: 10_000 + removed,
            after_bytes: 10_000,
            items: 1,
            tools: BTreeMap::new(),
            input_tokens: None,
            output_tokens: None,
            reason: None,
        }
    }

    /// Spec 123 T03: o que cada evento tirou conta uma vez por chamada seguinte da mesma sessão,
    /// a partir do passo do evento (inclusive), e só no histórico do evento: quando o `step` volta
    /// a 0, o `implement` recomeçou com outro histórico.
    #[test]
    fn reinjection_counts_later_calls_of_the_same_history() {
        // Histórico 1: passos 0 a 5 (s 0..5); histórico 2 da mesma sessão: passos 0 a 3 (s 100..103).
        let mut records: Vec<UsageRecord> = (0..6).map(|i| call("s1", "dev:T01:a1", i, u64::from(i))).collect();
        records.extend((0..4).map(|i| call("s1", "dev:T01:a1", i, 100 + u64::from(i))));
        // Outra sessão da spec, que não conta para os eventos da primeira.
        records.extend((0..3).map(|i| call("s1", "rev:T01:a1", i, 50 + u64::from(i))));
        let events = [
            // Passo 3 do histórico 1 (depois da chamada 2, no segundo 2): passos 3, 4 e 5 = 3 chamadas.
            event("s1", "dev:T01:a1", "compact", 3, 2, 400),
            // Passo 2 do histórico 2 (no segundo 101): passos 2 e 3 = 2 chamadas.
            event("s1", "dev:T01:a1", "spill", 2, 101, 800),
            // Passo 0 do histórico 2 (antes da primeira chamada, no segundo 99): 4 chamadas.
            event("s1", "dev:T01:a1", "dedup", 0, 99, 40),
            // Falha do resumo: nada sai do histórico.
            event("s1", "dev:T01:a1", "summary_failed", 4, 3, 9_000),
            // Sessão sem chamadas medidas: 0.
            event("s1", "ghost:T09:a1", "compact", 3, 2, 9_000),
        ];
        let tokens = reinjection_avoided_tokens(&records, &events);
        // (400 * 3 + 800 * 2 + 40 * 4) / 4 = (1200 + 1600 + 160) / 4 = 740.
        assert_eq!(tokens.get(&Some("s1".to_string())), Some(&740));
    }

    /// Spec 123 T03: o resumo refaz o histórico, então o efeito de um evento anterior para no passo
    /// do resumo, e o resumo conta o que tirou dali em diante.
    #[test]
    fn reinjection_of_earlier_events_stops_at_the_summary() {
        let records: Vec<UsageRecord> = (0..10).map(|i| call("s1", "dev:T01:a1", i, u64::from(i))).collect();
        let events = [
            event("s1", "dev:T01:a1", "compact", 2, 1, 100),
            // A chamada do resumo é o passo 5; o histórico novo vai a partir do passo 6.
            event("s1", "dev:T01:a1", "summary", 6, 5, 1000),
            // Compactação depois do resumo: não para no resumo.
            event("s1", "dev:T01:a1", "compact", 8, 7, 20),
        ];
        let tokens = reinjection_avoided_tokens(&records, &events);
        // compact: passos 2 a 5 = 4; summary: passos 6 a 9 = 4; compact: passos 8 e 9 = 2.
        // (100 * 4 + 1000 * 4 + 20 * 2) / 4 = 4440 / 4 = 1110.
        assert_eq!(tokens.get(&Some("s1".to_string())), Some(&1110));
    }

    /// Spec 123 T03: a mesma sessão em specs diferentes são históricos diferentes, e registros
    /// antigos (sem `session` ou `step`) não entram.
    #[test]
    fn reinjection_is_per_spec_and_ignores_old_records() {
        let mut records = vec![call("s1", "dev:T01:a1", 0, 0), call("s1", "dev:T01:a1", 1, 1), call("s2", "dev:T01:a1", 0, 0)];
        let mut old = call("s2", "dev:T01:a1", 1, 1);
        old.step = None;
        old.session = None;
        records.push(old);
        let events = [event("s1", "dev:T01:a1", "compact", 1, 0, 400), event("s2", "dev:T01:a1", "compact", 1, 0, 400)];
        let tokens = reinjection_avoided_tokens(&records, &events);
        assert_eq!(tokens.get(&Some("s1".to_string())), Some(&100));
        assert_eq!(tokens.get(&Some("s2".to_string())), None, "sem chamada medida depois do evento");
        let mut view = aggregate(&records, &BTreeMap::new());
        add_context_events(&mut view, &events, &records);
        assert_eq!(view.context.reinjection_avoided_tokens, 100);
        assert_eq!((view.context.compactions, view.context.saved_bytes), (2, 800), "a soma de antes continua");
        let s1 = view.specs.iter().find(|s| s.spec.as_deref() == Some("s1")).expect("s1");
        assert_eq!(s1.context.reinjection_avoided_tokens, 100);
    }

    /// Spec 122 T01: a API de custos devolve o resumo do contexto por projeto e por spec.
    #[tokio::test]
    async fn usage_route_returns_the_context_summary() {
        use crate::config::{Config, Project};
        let root = tmp("ctx-api");
        fs::create_dir_all(root.join(".sdd/specs/s2")).expect("mkdir");
        let sink = FileUsageSink::new(&root, "p1");
        sink.record(with_context(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(1000, 5)), 4000, 0));
        sink.record(with_context(ev(Some("s1"), Some("T01"), "developer", "m", Usage::new(3000, 5)), 12000, 1));
        sink.record(ev(Some("s2"), Some("T01"), "reviewer", "m", Usage::new(500, 5)));
        sink.record_context(ctx_event(Some("s1"), "developer:T01:a1", 300, 100));
        sink.record_context(ctx_event(Some("s1"), "developer:T01:a1", 250, 150));
        // Spec 122 T06: o resumo bem-sucedido conta em `summaries`; a falha, não.
        let mut summary = ctx_event(Some("s1"), "developer:T01:a1", 1000, 400);
        summary.kind = ContextKind::Summary;
        sink.record_context(summary);
        let mut failed = ctx_event(Some("s1"), "developer:T01:a1", 0, 0);
        failed.kind = ContextKind::SummaryFailed;
        sink.record_context(failed);
        for kind in [ContextKind::Spill, ContextKind::Dedup] {
            let mut e = ctx_event(Some("s1"), "developer:T01:a1", 0, 0);
            e.kind = kind;
            sink.record_context(e);
        }
        let cfg = Config {
            projects: vec![Project {
                id: "p1".into(),
                name: "P".into(),
                path: root.to_string_lossy().into_owned(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }],
            ..Config::default()
        };
        let state = AppState::new(tmp("ctx-api-cfg").join("config.toml"), cfg, 3000);
        let get = |spec: Option<&str>| {
            get_usage(State(state.clone()), AxPath("p1".into()), Query(UsageQuery { spec: spec.map(String::from) }))
        };
        let json = |r: Response| async move {
            let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.expect("body");
            serde_json::from_slice::<serde_json::Value>(&b).expect("json")
        };
        let all = json(get(None).await.expect("ok")).await;
        let c = &all["context"];
        assert_eq!((c["calls"].as_u64(), c["input"].as_u64(), c["avg_input"].as_u64()), (Some(3), Some(4500), Some(1500)));
        assert_eq!((c["max_input"].as_u64(), c["max_context_bytes"].as_u64()), (Some(3000), Some(12000)));
        assert_eq!((c["compactions"].as_u64(), c["summaries"].as_u64(), c["saved_bytes"].as_u64()), (Some(2), Some(1), Some(900)));
        let s1 = all["specs"].as_array().and_then(|a| a.iter().find(|s| s["spec"] == "s1")).expect("s1");
        assert_eq!((s1["context"]["calls"].as_u64(), s1["context"]["compactions"].as_u64(), s1["context"]["summaries"].as_u64()), (Some(2), Some(2), Some(1)));
        let s2 = all["specs"].as_array().and_then(|a| a.iter().find(|s| s["spec"] == "s2")).expect("s2");
        assert_eq!((s2["context"]["compactions"].as_u64(), s2["context"]["max_context_bytes"].as_u64()), (Some(0), Some(0)));
        // Spec 123 T03: os campos novos. Os eventos estão no passo 7, depois das chamadas medidas
        // (passos 0 e 1): nenhuma chamada seguinte, nada reenviado a menos.
        assert_eq!((c["spills"].as_u64(), c["dedups"].as_u64(), c["cache_pct"].as_u64()), (Some(1), Some(1), Some(0)));
        assert_eq!(c["reinjection_avoided_tokens"].as_u64(), Some(0));
        let one = json(get(Some("s1")).await.expect("ok")).await;
        assert_eq!((one["context"]["calls"].as_u64(), one["context"]["saved_bytes"].as_u64()), (Some(2), Some(900)));
        assert_eq!(one["total"]["calls"], 2, "os totais de antes continuam");
    }
}
