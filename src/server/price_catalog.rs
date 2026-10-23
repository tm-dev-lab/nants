//! Catálogo público de preços (LiteLLM) usado só como ESTIMATIVA de custo (spec 032/T03).
//!
//! GET simples, sem dados do usuário; cache em disco (TTL 24 h) com fallback para o cache antigo
//! se a rede falhar. Preço digitado pelo usuário (`manual`) nunca é sobrescrito.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::projects::format_utc;
use crate::config::{PriceSource, Pricing};

pub const CATALOG_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const CATALOG_TTL_SECS: u64 = 24 * 3600;
pub const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const CACHE_FILE: &str = "price-catalog.json";

pub type FetchFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send + 'a>>;

/// Obtém o corpo bruto do catálogo (injetável nos testes).
pub trait PriceFetcher: Send + Sync {
    fn fetch(&self) -> FetchFuture<'_>;
}

/// Fetcher de produção: reqwest/rustls, timeout, sem redirects, limite de tamanho.
pub struct HttpPriceFetcher;

impl PriceFetcher for HttpPriceFetcher {
    fn fetch(&self) -> FetchFuture<'_> {
        Box::pin(async {
            let client = reqwest::Client::builder()
                .timeout(FETCH_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "cliente HTTP".to_string())?;
            let mut resp = client.get(CATALOG_URL).send().await.map_err(|_| "rede".to_string())?;
            if !resp.status().is_success() {
                return Err(format!("status {}", resp.status().as_u16()));
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| "rede".to_string())? {
                if body.len() + chunk.len() > MAX_CATALOG_BYTES {
                    return Err("catálogo grande demais".to_string());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        })
    }
}

/// Fetcher fixo (testes e ambientes sem rede): devolve o corpo dado, ou falha se `None`.
pub struct StaticPriceFetcher(pub Option<Vec<u8>>);

impl PriceFetcher for StaticPriceFetcher {
    fn fetch(&self) -> FetchFuture<'_> {
        let r = self.0.clone().ok_or_else(|| "sem rede".to_string());
        Box::pin(async move { r })
    }
}

/// Preço por token convertido para US$ por 1M tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub input: f64,
    pub output: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached: Option<f64>,
    /// Janela de entrada do modelo em tokens (`max_input_tokens` do LiteLLM; spec 122 T04): base
    /// do limiar do resumo do histórico. Ausente no cache antigo ou no modelo sem o dado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Catalog {
    /// Data (AAAA-MM-DD) em que o catálogo foi obtido.
    pub date: String,
    pub fetched_at: u64,
    pub entries: BTreeMap<String, CatalogEntry>,
}

fn per_mtok(v: &serde_json::Value, key: &str) -> Option<f64> {
    let x = v.get(key)?.as_f64()?;
    (x.is_finite() && x >= 0.0).then(|| round_sig(x * 1_000_000.0))
}

/// Arredonda para 6 algarismos significativos (remove a cauda de ponto flutuante).
fn round_sig(x: f64) -> f64 {
    if x == 0.0 || !x.is_finite() {
        return x;
    }
    format!("{x:.5e}").parse().unwrap_or(x)
}

/// Interpreta o JSON do LiteLLM. Ignora `sample_spec` e entradas sem preço de entrada e saída.
pub fn parse_catalog(raw: &[u8]) -> Option<BTreeMap<String, CatalogEntry>> {
    let value: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let obj = value.as_object()?;
    let mut out = BTreeMap::new();
    for (name, v) in obj {
        if name == "sample_spec" {
            continue;
        }
        let (Some(input), Some(output)) =
            (per_mtok(v, "input_cost_per_token"), per_mtok(v, "output_cost_per_token"))
        else {
            continue;
        };
        out.insert(
            name.clone(),
            CatalogEntry {
                input,
                output,
                cached: per_mtok(v, "cache_read_input_token_cost"),
                context: v.get("max_input_tokens").and_then(serde_json::Value::as_u64).filter(|n| *n > 0),
            },
        );
    }
    Some(out)
}

impl Catalog {
    /// Entrada do modelo: casamento exato; depois sem o prefixo de provedor (`openai/`).
    fn entry(&self, model: &str) -> Option<&CatalogEntry> {
        self.entries.get(model).or_else(|| {
            let (_, rest) = model.split_once('/')?;
            self.entries.get(rest)
        })
    }

    /// Janela de entrada do modelo em tokens, quando o catálogo a traz (spec 122 T04).
    pub fn context_window(&self, model: &str) -> Option<u64> {
        self.entry(model).and_then(|e| e.context)
    }

    /// Casamento exato; depois sem o prefixo de provedor (`openai/`). Sem aproximação.
    pub fn lookup(&self, model: &str) -> Option<Pricing> {
        let found = self.entry(model)?;
        Some(Pricing {
            input_per_mtok: found.input,
            output_per_mtok: found.output,
            cached_input_per_mtok: found.cached,
            web_search_per_call: None,
            image_per_unit: None,
            source: PriceSource::Estimated,
            estimated_at: Some(self.date.clone()),
        })
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Lê o cache só se for arquivo regular (nunca segue symlink) e pequeno o bastante.
fn read_cache(dir: &Path) -> Option<Catalog> {
    use std::io::Read;
    let path = dir.join(CACHE_FILE);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_CATALOG_BYTES as u64 * 2 {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(&path).ok()?.take(meta.len() + 1).read_to_string(&mut text).ok()?;
    serde_json::from_str(&text).ok()
}

static TMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Grava o cache de forma atômica: recusa destino symlink/não regular; tmp exclusivo (0600),
/// `sync_all` e `rename`; remove o tmp em caso de falha.
fn write_cache(dir: &Path, catalog: &Catalog) {
    use std::io::Write;
    let Ok(text) = serde_json::to_string(catalog) else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let dest = dir.join(CACHE_FILE);
    if let Ok(meta) = std::fs::symlink_metadata(&dest) {
        if !meta.file_type().is_file() {
            return;
        }
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let n = TMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{CACHE_FILE}.{}-{nanos}-{n}.tmp", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let Ok(mut file) = opts.open(&tmp) else { return };
    let ok = file.write_all(text.as_bytes()).is_ok() && file.sync_all().is_ok();
    drop(file);
    if !ok || std::fs::rename(&tmp, &dest).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Catálogo vigente: cache fresco (< TTL, salvo `force`); senão rede; se falhar, cache antigo.
/// `None` quando não há cache nem rede.
pub async fn load_catalog(
    fetcher: &dyn PriceFetcher,
    dir: &Path,
    now: u64,
    force: bool,
) -> Option<Catalog> {
    let dir_buf = dir.to_path_buf();
    let cached = tokio::task::spawn_blocking(move || read_cache(&dir_buf)).await.ok().flatten();
    if let Some(c) = &cached {
        if !force && now.saturating_sub(c.fetched_at) < CATALOG_TTL_SECS {
            return cached;
        }
    }
    let fresh = match fetcher.fetch().await {
        Ok(raw) => parse_catalog(&raw).filter(|e| !e.is_empty()),
        Err(_) => None,
    };
    match fresh {
        Some(entries) => {
            let catalog = Catalog {
                date: format_utc(now).chars().take(10).collect(),
                fetched_at: now,
                entries,
            };
            let (d, c) = (dir.to_path_buf(), catalog.clone());
            let _ = tokio::task::spawn_blocking(move || write_cache(&d, &c)).await;
            Some(catalog)
        }
        None => cached,
    }
}

/// Janelas de entrada dos modelos pelo catálogo já em cache (spec 122 T04), sem rede: o limiar do
/// resumo do histórico usa a do modelo do papel. Sem cache ou sem o dado, `None` (o padrão
/// declarado vale). Bloqueante: chamar fora do executor assíncrono.
pub fn cached_context_windows(dir: &Path, models: &[&str]) -> Vec<Option<u64>> {
    let catalog = read_cache(dir);
    models.iter().map(|m| catalog.as_ref().and_then(|c| c.context_window(m))).collect()
}

/// Atalho com o relógio real.
pub async fn current_catalog(fetcher: &dyn PriceFetcher, dir: &Path, force: bool) -> Option<Catalog> {
    load_catalog(fetcher, dir, now_secs(), force).await
}

/// Aplica estimativas ao mapa: preenche modelos de `models` sem preço e atualiza os já `estimated`
/// (todos os `estimated` se `refresh`). `manual` nunca muda. Devolve se algo mudou.
pub fn apply_estimates(
    pricing: &mut BTreeMap<String, Pricing>,
    models: &[String],
    catalog: &Catalog,
    refresh: bool,
) -> bool {
    let mut targets: Vec<String> = models.to_vec();
    if refresh {
        targets.extend(
            pricing.iter().filter(|(_, p)| p.source == PriceSource::Estimated).map(|(m, _)| m.clone()),
        );
    }
    let mut changed = false;
    for model in targets {
        if model.trim().is_empty() {
            continue;
        }
        if pricing.get(&model).is_some_and(|p| p.source == PriceSource::Manual) {
            continue;
        }
        if let Some(mut est) = catalog.lookup(&model) {
            // Spec 092 T03: o preço por imagem do usuário sobrevive à renovação da estimativa.
            est.image_per_unit = pricing.get(&model).and_then(|p| p.image_per_unit);
            if pricing.get(&model) != Some(&est) {
                pricing.insert(model, est);
                changed = true;
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const FIXTURE: &str = r#"{
      "sample_spec": {"input_cost_per_token": 1, "output_cost_per_token": 1},
      "gpt-4o": {"input_cost_per_token": 0.0000025, "output_cost_per_token": 0.00001, "cache_read_input_token_cost": 0.00000125, "max_input_tokens": 128000},
      "claude-x": {"input_cost_per_token": 0.000003, "output_cost_per_token": 0.000015},
      "embed": {"input_cost_per_token": 0.0000001},
      "free": {"mode": "chat"}
    }"#;

    pub struct Fake {
        calls: Arc<AtomicUsize>,
        body: Option<Vec<u8>>,
    }
    impl PriceFetcher for Fake {
        fn fetch(&self) -> FetchFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let r = self.body.clone().ok_or_else(|| "rede".to_string());
            Box::pin(async move { r })
        }
    }
    fn fake(body: Option<&str>) -> (Fake, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (Fake { calls: Arc::clone(&calls), body: body.map(|b| b.as_bytes().to_vec()) }, calls)
    }
    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nants-pc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// Spec 122 T04: a janela de entrada vem do catálogo em cache, sem rede; sem o dado, `None`.
    #[tokio::test]
    async fn janela_do_modelo_vem_do_catalogo_em_cache() {
        let m = parse_catalog(FIXTURE.as_bytes()).expect("parse");
        assert_eq!(m["gpt-4o"].context, Some(128_000));
        assert_eq!(m["claude-x"].context, None);
        let dir = tmp("janela");
        assert_eq!(cached_context_windows(&dir, &["gpt-4o"]), vec![None], "sem cache");
        let (f, _) = fake(Some(FIXTURE));
        load_catalog(&f, &dir, 1_000, false).await.expect("catálogo");
        assert_eq!(
            cached_context_windows(&dir, &["gpt-4o", "openai/gpt-4o", "claude-x", "outro"]),
            vec![Some(128_000), Some(128_000), None, None]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_converte_por_token_para_mtok_e_ignora_invalidos() {
        let m = parse_catalog(FIXTURE.as_bytes()).expect("parse");
        assert_eq!(m.len(), 2, "sample_spec, sem saída e sem preço ficam fora");
        let g = &m["gpt-4o"];
        assert!((g.input - 2.5).abs() < 1e-9 && (g.output - 10.0).abs() < 1e-9);
        assert!((g.cached.unwrap_or(0.0) - 1.25).abs() < 1e-9);
        assert_eq!(m["claude-x"].cached, None);
        let tail = parse_catalog(br#"{"m":{"input_cost_per_token":1e-7,"output_cost_per_token":0.0000001}}"#).expect("p");
        assert_eq!(tail["m"].input, 0.1, "sem cauda de float");
        assert!(parse_catalog(b"nope").is_none());
    }

    #[test]
    fn casamento_exato_depois_sem_prefixo_e_sem_aproximacao() {
        let entries = parse_catalog(FIXTURE.as_bytes()).expect("parse");
        let c = Catalog { date: "2026-10-15".into(), fetched_at: 0, entries };
        assert!(c.lookup("gpt-4o").is_some());
        let p = c.lookup("openai/gpt-4o").expect("sem prefixo");
        assert_eq!(p.source, PriceSource::Estimated);
        assert_eq!(p.estimated_at.as_deref(), Some("2026-10-15"));
        assert!(c.lookup("gpt-4o-mini").is_none());
        assert!(c.lookup("gpt").is_none());
    }

    #[tokio::test]
    async fn cache_ttl_e_fallback() {
        let dir = tmp("cache");
        let (f, calls) = fake(Some(FIXTURE));
        let c = load_catalog(&f, &dir, 1_000_000, false).await.expect("rede");
        assert_eq!(c.entries.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        load_catalog(&f, &dir, 1_000_100, false).await.expect("cache");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "dentro do TTL não vai à rede");
        load_catalog(&f, &dir, 1_000_100, true).await.expect("força");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (down, dcalls) = fake(None);
        let late = 1_000_100 + CATALOG_TTL_SECS + 5;
        let old = load_catalog(&down, &dir, late, false).await.expect("fallback");
        assert_eq!(dcalls.load(Ordering::SeqCst), 1);
        assert_eq!(old.fetched_at, 1_000_100);
        assert!(load_catalog(&down, &tmp("vazio"), 5, false).await.is_none());
        let (bad, _) = fake(Some("lixo"));
        assert!(load_catalog(&bad, &dir, late, false).await.is_some(), "corpo inválido usa o cache");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_no_lugar_do_cache_e_ignorado_e_alvo_fica_intacto() {
        let dir = tmp("symlink");
        std::fs::create_dir_all(&dir).expect("dir");
        let target = dir.join("alvo.txt");
        std::fs::write(&target, "intacto").expect("alvo");
        std::os::unix::fs::symlink(&target, dir.join(CACHE_FILE)).expect("link");
        let (f, calls) = fake(Some(FIXTURE));
        let c = load_catalog(&f, &dir, 1_000, false).await.expect("usa o fetcher");
        assert_eq!(c.entries.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(std::fs::read_to_string(&target).expect("alvo"), "intacto");
        let tmps = std::fs::read_dir(&dir).expect("ls").flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(tmps, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn cache_corrompido_e_ignorado() {
        let dir = tmp("corrupt");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(CACHE_FILE), "{ lixo").expect("w");
        let (f, calls) = fake(Some(FIXTURE));
        assert!(load_catalog(&f, &dir, 1_000, false).await.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(read_cache(&dir).is_some(), "regravado com conteúdo válido");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nao_sobrescreve_manual_e_preenche_so_o_que_falta() {
        let entries = parse_catalog(FIXTURE.as_bytes()).expect("parse");
        let c = Catalog { date: "d".into(), fetched_at: 0, entries };
        let manual = Pricing {
            input_per_mtok: 9.0,
            output_per_mtok: 9.0,
            cached_input_per_mtok: None,
            web_search_per_call: None,
            image_per_unit: None,
            source: PriceSource::Manual,
            estimated_at: None,
        };
        let mut map = BTreeMap::from([("gpt-4o".to_string(), manual.clone())]);
        let models = vec!["gpt-4o".to_string(), "claude-x".to_string(), "desconhecido".to_string()];
        assert!(apply_estimates(&mut map, &models, &c, true));
        assert_eq!(map["gpt-4o"], manual);
        assert_eq!(map["claude-x"].source, PriceSource::Estimated);
        assert!(!map.contains_key("desconhecido"));
        assert!(!apply_estimates(&mut map, &models, &c, true), "idempotente");
        // Spec 092 T03: preço por imagem numa estimativa sobrevive à renovação.
        map.get_mut("claude-x").expect("estimado").image_per_unit = Some(0.04);
        assert!(!apply_estimates(&mut map, &models, &c, true));
        assert_eq!(map["claude-x"].image_per_unit, Some(0.04));
    }
}
