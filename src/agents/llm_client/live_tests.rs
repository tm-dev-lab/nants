//! Apoio dos testes ao vivo por provedor (spec 132 T11): a chave, o modelo, o preço e o teto de
//! gasto. O roteiro de uso está em `docs/live-tests.md`.
//!
//! - **Chave:** lida só aqui, do arquivo em `NANTS_LIVE_ANTHROPIC_KEY_FILE` ou
//!   `NANTS_LIVE_GEMINI_KEY_FILE`, e nunca impressa nem gravada em outro lugar.
//! - **Modelo:** escolhido na hora pela Models API do provedor (o Haiku mais recente na Anthropic; o
//!   Flash mais recente com raciocínio no Gemini), sem nome fixo no código. A variável
//!   `NANTS_LIVE_<PROVEDOR>_MODEL` troca o modelo.
//! - **Preço:** o do catálogo do Nants ([`crate::server::price_catalog`], pelo tipo da conexão),
//!   antes de qualquer pedido de modelo. Sem o catálogo, vale `NANTS_LIVE_<PROVEDOR>_PRICES`
//!   (`entrada,saída[,cache lido[,escrita de cache]]`, em US$ por 1 milhão de tokens). Sem nenhum
//!   dos dois, o teste falha antes de gastar.
//! - **Teto:** o custo de cada resposta sai do `Usage` e das buscas pela mesma conta da tela
//!   ([`crate::server::usage::cost_of`]) e soma por provedor em toda a execução. Passou do teto
//!   (`NANTS_LIVE_MAX_USD`, padrão US$ 0,05 por provedor) ou do limite de buscas
//!   (`NANTS_LIVE_MAX_SEARCHES`, padrão 3 por provedor), o teste falha e não faz mais pedidos.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{json, Value};

use super::{ChatResponse, LlmClient, Pacer, Usage};
use crate::config::{ConnectionKind, Pricing, PriceSource};

/// Teto padrão de gasto por provedor numa execução, em US$.
pub(super) const DEFAULT_MAX_USD: f64 = 0.05;

/// Limite padrão de buscas web por provedor numa execução.
pub(super) const DEFAULT_MAX_SEARCHES: u32 = 3;

/// Provedor de um teste ao vivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Provider {
    Anthropic,
    Gemini,
}

impl Provider {
    fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    fn kind(self) -> ConnectionKind {
        match self {
            Self::Anthropic => ConnectionKind::Anthropic,
            Self::Gemini => ConnectionKind::GoogleGemini,
        }
    }

    fn var(self, suffix: &str) -> String {
        let name = match self {
            Self::Anthropic => "ANTHROPIC",
            Self::Gemini => "GEMINI",
        };
        format!("NANTS_LIVE_{name}_{suffix}")
    }

    fn default_base(self) -> &'static str {
        match self {
            Self::Anthropic => crate::config::ANTHROPIC_BASE_URL,
            Self::Gemini => crate::config::GOOGLE_GEMINI_BASE_URL,
        }
    }
}

/// Gasto acumulado de um provedor na execução.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct Spend {
    pub calls: u32,
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub searches: u32,
    pub usd: f64,
}

impl Spend {
    fn add(&mut self, u: Usage, searches: u32, usd: f64) {
        self.calls += 1;
        self.prompt += u64::from(u.prompt);
        self.completion += u64::from(u.completion);
        self.cached += u64::from(u.cached.unwrap_or(0));
        self.cache_write += u64::from(u.cache_write.unwrap_or(0));
        self.reasoning += u64::from(u.reasoning.unwrap_or(0));
        self.searches += searches;
        self.usd += usd;
    }

    fn line(&self) -> String {
        format!(
            "{} pedidos; entrada {} (cache lido {}, gravado {}); saída {} (raciocínio {}); buscas {}; custo estimado US$ {:.5}",
            self.calls, self.prompt, self.cached, self.cache_write, self.completion, self.reasoning, self.searches, self.usd
        )
    }
}

/// Gasto por provedor em toda a execução (os testes de um provedor somam no mesmo teto).
static SPENT: Mutex<BTreeMap<Provider, Spend>> = Mutex::new(BTreeMap::new());

fn spent(provider: Provider) -> Spend {
    SPENT.lock().unwrap_or_else(PoisonError::into_inner).get(&provider).copied().unwrap_or_default()
}

/// Se ainda cabe outro teste do provedor na execução: o gasto abaixo do teto e as buscas dentro do
/// limite. Um teste que passou de um deles já falhou; os seguintes falham aqui, antes de qualquer
/// pedido pago.
pub(super) fn budget_left(spent: &Spend, max_usd: f64, max_searches: u32) -> Result<(), String> {
    if spent.usd >= max_usd {
        return Err(format!("o teto de US$ {max_usd} já foi atingido nesta execução (gasto US$ {:.5})", spent.usd));
    }
    if spent.searches > max_searches {
        return Err(format!("o limite de {max_searches} buscas já foi passado nesta execução ({} buscas)", spent.searches));
    }
    Ok(())
}

/// Teto em US$ a partir do valor de `NANTS_LIVE_MAX_USD`: ausente vale o padrão; inválido é erro.
pub(super) fn parse_max_usd(raw: Option<&str>) -> Result<f64, String> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(DEFAULT_MAX_USD),
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| format!("NANTS_LIVE_MAX_USD inválido: {v:?} (um número maior que zero, em US$)")),
    }
}

/// Limite de buscas a partir do valor de `NANTS_LIVE_MAX_SEARCHES`: ausente vale o padrão.
pub(super) fn parse_max_searches(raw: Option<&str>) -> Result<u32, String> {
    match raw.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(DEFAULT_MAX_SEARCHES),
        Some(v) => v.parse::<u32>().map_err(|_| format!("NANTS_LIVE_MAX_SEARCHES inválido: {v:?}")),
    }
}

/// Preços passados por variável: `entrada,saída[,cache lido[,escrita de cache]]` em US$ por 1M.
pub(super) fn parse_prices(raw: &str) -> Option<Pricing> {
    let parts: Vec<f64> = raw.split(',').map(|p| p.trim().parse::<f64>().ok()).collect::<Option<Vec<f64>>>()?;
    if !(2..=4).contains(&parts.len()) || parts.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return None;
    }
    Some(Pricing {
        input_per_mtok: parts[0],
        output_per_mtok: parts[1],
        cached_input_per_mtok: parts.get(2).copied(),
        cache_write_per_mtok: parts.get(3).copied(),
        web_search_per_call: None,
        image_per_unit: None,
        source: PriceSource::Manual,
        estimated_at: None,
    })
}

/// Custo em US$ de uma resposta pela conta da tela ([`crate::server::usage::cost_of`]).
pub(super) fn cost(model: &str, pricing: &Pricing, u: Usage, searches: u32) -> f64 {
    let record = json!({
        "at": "",
        "project": "live",
        "agent": "live",
        "model": model,
        "input": u.prompt,
        "output": u.completion,
        "cached": u.cached,
        "cache_write": u.cache_write,
        "web_search_calls": searches,
    });
    let Ok(record) = serde_json::from_value::<crate::server::usage::UsageRecord>(record) else {
        panic!("registro de uso inválido");
    };
    let map = BTreeMap::from([(model.to_string(), pricing.clone())]);
    crate::server::usage::cost_of(&record, &map).unwrap_or(f64::INFINITY)
}

/// O Haiku mais recente da lista da Anthropic (`GET /v1/models`), pelo `created_at`.
pub(super) fn pick_latest_haiku(list: &Value) -> Option<String> {
    list["data"]
        .as_array()?
        .iter()
        .filter(|m| m["id"].as_str().is_some_and(|id| id.contains("haiku")))
        .max_by(|a, b| a["created_at"].as_str().unwrap_or_default().cmp(b["created_at"].as_str().unwrap_or_default()))
        .and_then(|m| m["id"].as_str())
        .map(str::to_string)
}

/// O Flash mais recente com raciocínio da lista do Gemini (`GET /v1beta/models`): `thinking: true`,
/// `generateContent` e o nome `gemini-<versão>-flash` (sem lite, imagem, áudio ou prévia), a maior
/// versão.
pub(super) fn pick_latest_flash(list: &Value) -> Option<String> {
    let mut best: Option<(Vec<u32>, String)> = None;
    for m in list["models"].as_array().into_iter().flatten() {
        let name = m["name"].as_str().unwrap_or_default().trim_start_matches("models/");
        let generates = m["supportedGenerationMethods"].as_array().is_some_and(|g| g.iter().any(|x| x == "generateContent"));
        let Some(version) = name.strip_prefix("gemini-").and_then(|n| n.strip_suffix("-flash")) else {
            continue;
        };
        let Some(parts) = version.split('.').map(|p| p.parse::<u32>().ok()).collect::<Option<Vec<u32>>>() else {
            continue;
        };
        if m["thinking"] == true && generates && best.as_ref().is_none_or(|(v, _)| parts > *v) {
            best = Some((parts, name.to_string()));
        }
    }
    best.map(|(_, n)| n)
}

/// Marca desta execução (o instante de início), a mesma em todos os pedidos dela.
fn run_tag() -> u128 {
    static TAG: std::sync::OnceLock<u128> = std::sync::OnceLock::new();
    *TAG.get_or_init(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0))
}

/// Um prefixo longo e estável, para o cache do provedor: `lines` linhas de uns 35 tokens cada.
/// - `per_run`: a primeira linha leva a marca da execução, para o cache começar vazio e o teste
///   provar a gravação e a leitura (Anthropic, cache explícito e imediato);
/// - sem ela, o prefixo é o mesmo entre execuções (Gemini: o cache implícito não fica pronto dentro
///   de uma execução curta, mas uma execução seguinte o encontra).
pub(super) fn long_prefix(lines: usize, per_run: bool) -> String {
    let head = if per_run { format!("Run {}. ", run_tag()) } else { String::new() };
    let mut text = format!("{head}You are a terse assistant. The reference notes below are background only; never quote them.\n");
    for i in 0..lines {
        text.push_str(&format!(
            "Reference note {i:04}: glossary entry {i} maps the term T{i} to the definition D{i}, kept only to make this prefix long and stable.\n"
        ));
    }
    text
}

/// Sessão de um provedor num teste ao vivo.
pub(super) struct Live {
    pub client: LlmClient,
    pub model: String,
    pub base: String,
    /// Id da conexão no ritmo (089): o teste lê os tetos que os cabeçalhos ensinaram.
    pub conn: &'static str,
    pub pacer: Arc<Pacer>,
    provider: Provider,
    pricing: Pricing,
    max_usd: f64,
    max_searches: u32,
}

impl Live {
    /// Abre a sessão: a chave do arquivo, o cliente sem novas tentativas (com o ritmo ligado), o
    /// modelo pela Models API e o preço. Falha antes de qualquer pedido de modelo quando falta a
    /// chave, o preço ou um teto válido, ou quando o provedor já passou do teto na execução.
    pub async fn open(provider: Provider) -> Self {
        let max_usd = parse_max_usd(std::env::var("NANTS_LIVE_MAX_USD").ok().as_deref()).unwrap_or_else(|e| panic!("{e}"));
        let max_searches =
            parse_max_searches(std::env::var("NANTS_LIVE_MAX_SEARCHES").ok().as_deref()).unwrap_or_else(|e| panic!("{e}"));
        if let Err(e) = budget_left(&spent(provider), max_usd, max_searches) {
            panic!("{}: {e}", provider.label());
        }
        let key_var = provider.var("KEY_FILE");
        let Ok(path) = std::env::var(&key_var) else {
            panic!("defina {key_var} com o caminho do arquivo da chave");
        };
        // O erro de leitura não leva o conteúdo; a chave nunca é impressa.
        let key = match std::fs::read_to_string(&path) {
            Ok(k) => k.trim().to_string(),
            Err(e) => panic!("leitura do arquivo em {key_var}: {}", e.kind()),
        };
        assert!(!key.is_empty(), "arquivo da chave vazio ({key_var})");
        let base = std::env::var(provider.var("BASE")).unwrap_or_else(|_| provider.default_base().to_string());
        let conn = match provider {
            Provider::Anthropic => "live-anthropic",
            Provider::Gemini => "live-gemini",
        };
        let pacer = Pacer::global();
        let client = LlmClient::new(base.clone(), key)
            .expect("cliente")
            .with_kind(provider.kind())
            .with_retry_delays(Vec::new())
            .with_throttle_delays(Vec::new())
            .with_pacer(Arc::clone(&pacer), conn);
        let model = match std::env::var(provider.var("MODEL")) {
            Ok(m) if !m.trim().is_empty() => m.trim().to_string(),
            _ => Self::pick_model(&client, provider).await,
        };
        let pricing = Self::pricing(provider, &model).await;
        eprintln!(
            "[{}] modelo {model}; US$ por 1M: entrada {}, saída {}, cache lido {:?}, escrita de cache {:?}; busca US$ {}; teto US$ {max_usd}, até {max_searches} buscas",
            provider.label(),
            pricing.input_per_mtok,
            pricing.output_per_mtok,
            pricing.cached_input_per_mtok,
            pricing.cache_write_per_mtok,
            pricing.web_search_price(),
        );
        Self { client, model, base, conn, pacer, provider, pricing, max_usd, max_searches }
    }

    /// O modelo pela Models API do provedor (sem custo de tokens).
    async fn pick_model(client: &LlmClient, provider: Provider) -> String {
        match provider {
            Provider::Anthropic => {
                let list = client.get_json("/v1/models?limit=100").await.expect("lista de modelos da Anthropic");
                pick_latest_haiku(&list).expect("nenhum Haiku na lista de modelos")
            }
            Provider::Gemini => {
                let list = client.get_json("/v1beta/models?pageSize=1000").await.expect("lista de modelos do Gemini");
                pick_latest_flash(&list).expect("nenhum Flash com raciocínio na lista de modelos")
            }
        }
    }

    /// O preço do catálogo do Nants pelo tipo da conexão; sem ele, o da variável de preços.
    async fn pricing(provider: Provider, model: &str) -> Pricing {
        use crate::server::price_catalog::{current_catalog, HttpPriceFetcher};
        // Pasta própria dentro do `TMPDIR` do `scripts/test.sh`, apagada no fim da rodada.
        let dir = std::env::temp_dir().join("nants-live-prices");
        let from_catalog = current_catalog(&HttpPriceFetcher, &dir, false).await.and_then(|c| c.lookup_for(model, provider.kind()));
        if let Some(p) = from_catalog {
            return p;
        }
        let var = provider.var("PRICES");
        match std::env::var(&var).ok().as_deref().and_then(parse_prices) {
            Some(p) => p,
            None => panic!("sem preço para {model} no catálogo: defina {var}=entrada,saída[,cache lido[,escrita de cache]] (US$ por 1M)"),
        }
    }

    /// Soma o uso e as buscas da resposta ao gasto do provedor, imprime a linha do pedido e falha
    /// quando o gasto passa do teto ou as buscas passam do limite.
    pub fn record(&self, label: &str, r: &ChatResponse) {
        let u = r.usage.unwrap_or(Usage::new(0, 0));
        let usd = cost(&self.model, &self.pricing, u, r.web_search_calls);
        let total = {
            let mut all = SPENT.lock().unwrap_or_else(PoisonError::into_inner);
            let s = all.entry(self.provider).or_default();
            s.add(u, r.web_search_calls, usd);
            *s
        };
        eprintln!(
            "[{}] {label}: entrada {} (cache lido {:?}, gravado {:?}); saída {} (raciocínio {:?}); buscas {}; US$ {usd:.5}; acumulado US$ {:.5}",
            self.provider.label(),
            u.prompt,
            u.cached,
            u.cache_write,
            u.completion,
            u.reasoning,
            r.web_search_calls,
            total.usd
        );
        assert!(total.usd <= self.max_usd, "{}: gasto US$ {:.5} passou do teto de US$ {}", self.provider.label(), total.usd, self.max_usd);
        assert!(
            total.searches <= self.max_searches,
            "{}: {} buscas passaram do limite de {}",
            self.provider.label(),
            total.searches,
            self.max_searches
        );
    }

    /// Imprime o total do provedor na execução até aqui.
    pub fn report(&self) {
        eprintln!("[{}] total da execução: {}", self.provider.label(), spent(self.provider).line());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teto_padrao_e_ajustavel() {
        assert_eq!(parse_max_usd(None), Ok(0.05));
        assert_eq!(parse_max_usd(Some(" ")), Ok(0.05));
        assert_eq!(parse_max_usd(Some("0.2")), Ok(0.2));
        for bad in ["0", "-1", "abc", "inf", "NaN"] {
            assert!(parse_max_usd(Some(bad)).is_err(), "{bad}");
        }
        assert_eq!(parse_max_searches(None), Ok(3));
        assert_eq!(parse_max_searches(Some("0")), Ok(0));
        assert!(parse_max_searches(Some("-1")).is_err());
    }

    #[test]
    fn orcamento_antes_de_abrir() {
        let spend = |usd: f64, searches: u32| Spend { usd, searches, ..Spend::default() };
        assert_eq!(budget_left(&spend(0.0, 0), 0.05, 3), Ok(()));
        assert_eq!(budget_left(&spend(0.049, 3), 0.05, 3), Ok(()), "abaixo do teto e no limite de buscas");
        let at_ceiling = budget_left(&spend(0.05, 0), 0.05, 3).expect_err("no teto");
        assert!(at_ceiling.contains("teto"), "{at_ceiling}");
        let over = budget_left(&spend(0.01, 4), 0.05, 3).expect_err("buscas acima do limite");
        assert!(over.contains("buscas"), "{over}");
        assert_eq!(budget_left(&spend(0.0, 1), 0.05, 0).map_err(|e| e.contains("buscas")), Err(true));
    }

    #[test]
    fn precos_por_variavel() {
        let p = parse_prices("1, 5, 0.1, 1.25").expect("preço");
        assert_eq!((p.input_per_mtok, p.output_per_mtok, p.cached_input_per_mtok, p.cache_write_per_mtok), (1.0, 5.0, Some(0.1), Some(1.25)));
        let p = parse_prices("0.75,3.75").expect("preço");
        assert_eq!((p.cached_input_per_mtok, p.cache_write_per_mtok), (None, None));
        for bad in ["", "1", "1,x", "1,-2", "1,2,3,4,5"] {
            assert!(parse_prices(bad).is_none(), "{bad}");
        }
    }

    /// O custo segue a conta da tela: a leitura e a escrita de cache com os preços delas, a saída
    /// (que inclui o raciocínio) e as buscas pelo preço por chamada.
    #[test]
    fn custo_pela_conta_da_tela() {
        let p = parse_prices("1,5,0.1,1.25").expect("preço");
        let u = Usage { prompt: 10_000, completion: 1_000, cached: Some(4_000), reasoning: Some(300), cache_write: Some(2_000) };
        let usd = cost("m", &p, u, 2);
        let expected = (4_000.0 * 1.0 + 4_000.0 * 0.1 + 2_000.0 * 1.25 + 1_000.0 * 5.0) / 1_000_000.0 + 2.0 * 0.01;
        assert!((usd - expected).abs() < 1e-12, "{usd} {expected}");
    }

    #[test]
    fn haiku_mais_recente_pela_lista() {
        let list = json!({"data": [
            {"id": "claude-haiku-velho", "created_at": "2025-10-01T00:00:00Z"},
            {"id": "claude-sonnet-novo", "created_at": "2026-12-01T00:00:00Z"},
            {"id": "claude-haiku-novo", "created_at": "2026-09-01T00:00:00Z"}
        ]});
        assert_eq!(pick_latest_haiku(&list).as_deref(), Some("claude-haiku-novo"));
        assert_eq!(pick_latest_haiku(&json!({"data": [{"id": "claude-opus"}]})), None);
    }

    #[test]
    fn flash_mais_recente_com_raciocinio() {
        let model = |name: &str, thinking: bool| json!({"name": format!("models/{name}"), "thinking": thinking, "supportedGenerationMethods": ["generateContent"]});
        let list = json!({"models": [
            model("gemini-3.7-flash", true),
            model("gemini-3.10-flash", false),
            model("gemini-3.9-flash-lite", true),
            model("gemini-3.8-flash", true),
            {"name": "models/gemini-3.9-flash", "thinking": true, "supportedGenerationMethods": ["embedContent"]}
        ]});
        assert_eq!(pick_latest_flash(&list).as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(pick_latest_flash(&json!({"models": []})), None);
    }

    #[test]
    fn prefixo_longo_e_estavel() {
        assert_eq!(long_prefix(3, true), long_prefix(3, true));
        assert!(long_prefix(3, true).starts_with("Run "));
        assert!(long_prefix(3, false).starts_with("You are"));
        assert!(long_prefix(200, false).len() > 20_000);
    }
}
