use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Configuração global do usuário (`~/.config/nants/config.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub connections: Vec<Connection>,
    pub agents: Agents,
    pub projects: Vec<Project>,
    /// Preço por modelo (`[pricing."<modelo>"]`), definido pelo usuário. Sem entrada, sem custo.
    pub pricing: BTreeMap<String, Pricing>,
    /// Última escolha do modo YOLO (spec 064): projeto novo nasce com esse valor. Ausente = false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub last_yolo: bool,
    /// Limites da internet do Arquiteto (`[architect_internet]`, spec 076). Tudo opcional.
    #[serde(skip_serializing_if = "ArchitectInternet::is_default")]
    pub architect_internet: ArchitectInternet,
    /// Raiz da pasta de projetos gerenciada (spec 093 T02). Ausente = [`DEFAULT_PROJECTS_ROOT`].
    /// Aceita `~` no início (expandido para o `$HOME`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_root: Option<String>,
}

/// Raiz padrão da pasta de projetos: visível (não oculta), para abrir no editor e no explorador.
pub const DEFAULT_PROJECTS_ROOT: &str = "~/NantsProjects";

/// Limites por turno de fase da internet do Arquiteto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArchitectInternet {
    pub plan_searches: u32,
    pub plan_pages: u32,
    pub plan_lookups: u32,
    pub recovery_searches: u32,
    pub recovery_pages: u32,
    pub recovery_lookups: u32,
}

impl Default for ArchitectInternet {
    fn default() -> Self {
        Self {
            plan_searches: 5,
            plan_pages: 8,
            plan_lookups: 10,
            recovery_searches: 3,
            recovery_pages: 5,
            recovery_lookups: 10,
        }
    }
}

impl ArchitectInternet {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// `(buscas, páginas, consultas)` do plano.
    pub fn plan(&self) -> (u32, u32, u32) {
        (self.plan_searches, self.plan_pages, self.plan_lookups)
    }

    /// `(buscas, páginas, consultas)` da recuperação.
    pub fn recovery(&self) -> (u32, u32, u32) {
        (self.recovery_searches, self.recovery_pages, self.recovery_lookups)
    }
}

/// Preço de um modelo em US$ por 1 milhão de tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    /// Preço da entrada servida do cache; ausente = cobra como entrada normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_per_mtok: Option<f64>,
    /// Preço da entrada gravada no cache do provedor (spec 132 T02: a escrita de cache da
    /// Anthropic); ausente = cobra como entrada normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_mtok: Option<f64>,
    /// Preço em US$ de cada chamada de busca na web. Ausente = 0.01 (valor público da OpenAI:
    /// US$ 10 por 1.000 chamadas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search_per_call: Option<f64>,
    /// Preço em US$ de cada imagem gerada (spec 092 T03). Ausente = estimativa padrão por
    /// qualidade ([`default_image_price`]), marcada como estimada.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_per_unit: Option<f64>,
    /// Origem do preço; ausente (formato antigo) = `manual`.
    #[serde(default)]
    pub source: PriceSource,
    /// Data (AAAA-MM-DD) do catálogo de onde veio a estimativa; só com `source = estimated`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_at: Option<String>,
}

/// Origem de um preço: digitado pelo usuário ou estimado pelo catálogo público.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PriceSource {
    #[default]
    Manual,
    Estimated,
}

/// Preço padrão (US$) por chamada de busca na web: US$ 10 por 1.000 chamadas.
pub const DEFAULT_WEB_SEARCH_PER_CALL: f64 = 0.01;

/// Estimativa padrão (US$) por imagem gerada, por qualidade (spec 092 T03): `low` 0,02, `medium`
/// 0,06 e `high` 0,25. Qualidade ausente ou desconhecida = `medium` (o padrão da ferramenta).
pub fn default_image_price(quality: Option<&str>) -> f64 {
    match quality {
        Some("low") => 0.02,
        Some("high") => 0.25,
        _ => 0.06,
    }
}

impl Pricing {
    /// Preço efetivo por chamada de busca na web.
    pub fn web_search_price(&self) -> f64 {
        self.web_search_per_call.unwrap_or(DEFAULT_WEB_SEARCH_PER_CALL)
    }

    /// Preço efetivo por imagem e se ele é estimado (sem `image_per_unit`, vale o padrão por qualidade).
    pub fn image_price(&self, quality: Option<&str>) -> (f64, bool) {
        match self.image_per_unit {
            Some(v) => (v, false),
            None => (default_image_price(quality), true),
        }
    }

    /// Todos os valores finitos e não negativos.
    pub fn is_valid(&self) -> bool {
        [
            Some(self.input_per_mtok),
            Some(self.output_per_mtok),
            self.cached_input_per_mtok,
            self.cache_write_per_mtok,
            self.web_search_per_call,
            self.image_per_unit,
        ]
            .into_iter()
            .flatten()
            .all(|v| v.is_finite() && v >= 0.0)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            connections: Vec::new(),
            agents: Agents::default(),
            projects: Vec::new(),
            pricing: BTreeMap::new(),
            last_yolo: false,
            architect_internet: ArchitectInternet::default(),
            projects_root: None,
        }
    }
}

/// Parâmetros do servidor HTTP local.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
    /// Faixa das portas de visualização do app em `--lan` (spec 097). Ausente: as 10 portas
    /// seguintes à porta efetiva do Nants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_view_ports: Option<LanViewPortsConfig>,
    /// Acesso pela rede local (spec 111), ligado e desligado em Configurações > Acesso pela rede.
    /// Ausente: a pessoa nunca escolheu (o `nants` pode adotar o que o Windows já tem).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan: Option<bool>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3000,
            lan_view_ports: None,
            lan: None,
        }
    }
}

/// `lan_view_ports = { start, count }` em `[server]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LanViewPortsConfig {
    pub start: u16,
    pub count: u16,
}

/// Conexão global com uma API compatível com OpenAI.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
///
/// Não há tipos de provedor: toda conexão é OpenAI-compatível e o provedor é
/// definido pela `base_url`. Campos legados (`provider_type`, `api_version`) no
/// TOML são ignorados ao carregar e não são mais gravados.
///
/// Spec 116: `kind` diz como a conexão se autentica. Ausente (configuração antiga) é a conexão por
/// chave de hoje. A conexão pelo plano ChatGPT (`chatgpt_plan`) não tem `api_key`, tem a
/// `base_url` e o `wire_api` fixos ([`Connection::normalize`]) e guarda as credenciais num
/// arquivo próprio da pasta de dados (`server::chatgpt`).
pub struct Connection {
    pub id: String,
    pub name: String,
    pub base_url: String,
    /// Valor literal ou referência no formato `env:NOME_VARIAVEL`. Vazio (e fora do TOML) na
    /// conexão pelo plano ChatGPT.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// Protocolo de chat usado com esta conexão (padrão `chat`).
    #[serde(default)]
    pub wire_api: WireApi,
    /// Como a conexão se autentica (spec 116); a por chave não é gravada.
    #[serde(default, skip_serializing_if = "ConnectionKind::is_api_key")]
    pub kind: ConnectionKind,
    /// Endereço avançado das conexões nativas (Anthropic e Google Gemini, spec 132): substitui o
    /// endereço oficial fixo (ex.: um proxy da empresa). Ausente usa o oficial. Não vale nos demais
    /// tipos, e [`Connection::normalize`] o apaga neles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url_override: Option<String>,
}

/// Endereço fixo da conexão pelo plano ChatGPT (spec 116).
pub const CHATGPT_PLAN_BASE_URL: &str = "https://api.openai.com/v1";
/// Endereço oficial da API da Anthropic (spec 132).
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
/// Endereço oficial da API do Google Gemini pelo AI Studio (spec 132).
pub const GOOGLE_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// Como a conexão se autentica (spec 116).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    /// Chave de API (literal ou `env:NOME`): a conexão de sempre.
    #[default]
    ApiKey,
    /// Plano ChatGPT (Plus ou Pro), pelo login OAuth; sem chave.
    ChatgptPlan,
    /// API nativa da Anthropic (Messages), por chave (spec 132).
    Anthropic,
    /// API nativa do Google Gemini pelo AI Studio, por chave (spec 132).
    GoogleGemini,
}

impl ConnectionKind {
    pub fn is_api_key(&self) -> bool {
        *self == ConnectionKind::ApiKey
    }

    /// Forma textual usada na API e no TOML (`"api_key"`, `"chatgpt_plan"`, `"anthropic"` ou
    /// `"google_gemini"`).
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionKind::ApiKey => "api_key",
            ConnectionKind::ChatgptPlan => "chatgpt_plan",
            ConnectionKind::Anthropic => "anthropic",
            ConnectionKind::GoogleGemini => "google_gemini",
        }
    }

    /// Lê a forma textual de [`Self::as_str`]; `None` para um tipo desconhecido.
    pub fn parse(raw: &str) -> Option<Self> {
        [Self::ApiKey, Self::ChatgptPlan, Self::Anthropic, Self::GoogleGemini]
            .into_iter()
            .find(|k| k.as_str() == raw)
    }

    /// Provedor com API própria (spec 132): endereço oficial fixo, protocolo implícito e chave.
    pub fn is_native(self) -> bool {
        matches!(self, ConnectionKind::Anthropic | ConnectionKind::GoogleGemini)
    }

    /// Endereço oficial fixo do tipo; `None` na conexão por chave, cujo endereço é livre.
    pub fn fixed_base_url(self) -> Option<&'static str> {
        match self {
            ConnectionKind::ApiKey => None,
            ConnectionKind::ChatgptPlan => Some(CHATGPT_PLAN_BASE_URL),
            ConnectionKind::Anthropic => Some(ANTHROPIC_BASE_URL),
            ConnectionKind::GoogleGemini => Some(GOOGLE_GEMINI_BASE_URL),
        }
    }
}

impl Connection {
    /// Conexão pelo plano ChatGPT com a `base_url` e o `wire_api` fixos e sem chave.
    pub fn chatgpt_plan(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            base_url: CHATGPT_PLAN_BASE_URL.to_string(),
            api_key: String::new(),
            wire_api: WireApi::Responses,
            kind: ConnectionKind::ChatgptPlan,
            base_url_override: None,
        }
    }

    /// Conexão nativa (Anthropic ou Google Gemini, spec 132) com o endereço oficial fixo.
    pub fn native(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: ConnectionKind,
        api_key: impl Into<String>,
    ) -> Self {
        let mut conn = Self {
            id: id.into(),
            name: name.into(),
            base_url: String::new(),
            api_key: api_key.into(),
            wire_api: WireApi::Chat,
            kind,
            base_url_override: None,
        };
        conn.normalize();
        conn
    }

    pub fn is_chatgpt_plan(&self) -> bool {
        self.kind == ConnectionKind::ChatgptPlan
    }

    /// Endereço usado nas chamadas: o avançado ([`Self::base_url_override`]) quando houver, senão
    /// o `base_url`.
    pub fn endpoint(&self) -> &str {
        self.base_url_override.as_deref().unwrap_or(&self.base_url)
    }

    /// Fixa os campos que vêm do tipo (config editada à mão):
    /// - plano ChatGPT: `base_url` e `wire_api` fixos e nenhuma chave;
    /// - Anthropic e Google Gemini (spec 132): `base_url` oficial e `wire_api` neutro (`chat`; o
    ///   protocolo é o do tipo); o endereço avançado só fica se for válido ([`valid_override_url`]);
    /// - por chave: não muda, a não ser o endereço avançado, que não vale nela.
    pub fn normalize(&mut self) {
        match self.kind {
            ConnectionKind::ApiKey => {}
            ConnectionKind::ChatgptPlan => {
                self.wire_api = WireApi::Responses;
                self.api_key.clear();
            }
            ConnectionKind::Anthropic | ConnectionKind::GoogleGemini => self.wire_api = WireApi::Chat,
        }
        if let Some(url) = self.kind.fixed_base_url() {
            self.base_url = url.to_string();
        }
        self.base_url_override = match self.base_url_override.take() {
            Some(raw) if self.kind.is_native() => valid_override_url(&raw),
            _ => None,
        };
    }
}

/// Endereço avançado aceito (spec 132): `http` ou `https` com host, sem credenciais, consulta nem
/// fragmento. Devolve a forma normalizada (sem `/` final); `None` se vazio ou inválido.
pub fn valid_override_url(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none();
    ok.then(|| url.as_str().trim_end_matches('/').to_string())
}

/// Protocolo HTTP de uma conexão: Chat Completions ou Responses (`/v1/responses`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireApi {
    /// `POST {base_url}/chat/completions`.
    #[default]
    Chat,
    /// `POST {base_url}/responses`.
    Responses,
}

impl WireApi {
    /// Forma textual usada na API (`"chat"` ou `"responses"`).
    pub fn as_str(self) -> &'static str {
        match self {
            WireApi::Chat => "chat",
            WireApi::Responses => "responses",
        }
    }
}

/// `Debug` manual: redige `api_key` para não vazar segredos em logs.
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &"***")
            .field("wire_api", &self.wire_api)
            .field("kind", &self.kind)
            .field("base_url_override", &self.base_url_override)
            .finish()
    }
}

/// Nível de esforço do agente.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

/// Configuração dos três papéis de agente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Agents {
    pub architect: AgentConfig,
    pub developer: AgentConfig,
    pub reviewer: AgentConfig,
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            architect: AgentConfig::with_effort(Effort::Low),
            developer: AgentConfig::with_effort(Effort::Low),
            reviewer: AgentConfig::with_effort(Effort::Medium),
        }
    }
}

/// Roteamento de conexão, modelo e esforço de um agente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub connection_id: String,
    pub model: String,
    pub effort: Effort,
    /// Teto de passos (pedidos ao modelo) por sessão do agente (spec 081 T01); `None` = padrão do
    /// papel. Usado pelo Desenvolvedor e pelo Revisor; o Arquiteto ignora.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_steps: Option<u32>,
}

/// Faixa aceita para `max_steps` (spec 081 T01): o PUT de agentes rejeita valores fora dela.
pub const MAX_STEPS_RANGE: std::ops::RangeInclusive<u32> = 10..=1000;

impl AgentConfig {
    /// Teto de passos efetivo: o valor configurado, ajustado a [`MAX_STEPS_RANGE`] (defesa para
    /// config editada à mão), ou `default` sem valor.
    pub fn max_steps_or(&self, default: usize) -> usize {
        self.max_steps_clamped().unwrap_or(default)
    }

    /// Valor configurado ajustado a [`MAX_STEPS_RANGE`]; `None` sem valor.
    pub fn max_steps_clamped(&self) -> Option<usize> {
        self.max_steps
            .map(|n| n.clamp(*MAX_STEPS_RANGE.start(), *MAX_STEPS_RANGE.end()) as usize)
    }

    fn with_effort(effort: Effort) -> Self {
        Self {
            connection_id: String::new(),
            model: String::new(),
            effort,
            max_steps: None,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::with_effort(Effort::Low)
    }
}

/// Projeto local registrado no Nants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_expected_values() {
        let cfg = Config::default();
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 3000);
        assert!(cfg.connections.is_empty());
        assert!(cfg.projects.is_empty());
        assert_eq!(cfg.agents.architect.effort, Effort::Low);
        assert_eq!(cfg.agents.developer.effort, Effort::Low);
        assert_eq!(cfg.agents.reviewer.effort, Effort::Medium);
        assert!(cfg.agents.architect.model.is_empty());
        assert!(cfg.agents.architect.connection_id.is_empty());
    }

    #[test]
    fn pricing_round_trips_in_toml_and_validates() {
        let cfg: Config = toml::from_str(
            "[pricing.\"m-1\"]\ninput_per_mtok = 1.5\noutput_per_mtok = 6\n",
        )
        .expect("pricing");
        let p = cfg.pricing["m-1"].clone();
        assert_eq!(p.cached_input_per_mtok, None);
        assert_eq!(p.cache_write_per_mtok, None, "config antiga carrega sem a escrita de cache");
        assert!(p.is_valid());
        assert!(!toml::to_string(&cfg).expect("toml").contains("cache_write_per_mtok"), "ausente continua ausente");
        let with: Config = toml::from_str(
            "[pricing.\"m-1\"]\ninput_per_mtok = 3\noutput_per_mtok = 15\ncache_write_per_mtok = 3.75\n",
        )
        .expect("escrita de cache");
        assert_eq!(with.pricing["m-1"].cache_write_per_mtok, Some(3.75));
        let again: Config = toml::from_str(&toml::to_string(&with).expect("toml")).expect("reload");
        assert_eq!(again.pricing, with.pricing);
        assert!(!Pricing { cache_write_per_mtok: Some(-1.0), ..with.pricing["m-1"].clone() }.is_valid());
        let back: Config = toml::from_str(&toml::to_string(&cfg).expect("toml")).expect("reload");
        assert_eq!(back.pricing, cfg.pricing);
        assert!(!Pricing { input_per_mtok: -1.0, ..p.clone() }.is_valid());
        assert!(!Pricing { output_per_mtok: f64::NAN, ..p }.is_valid());
    }

    /// Spec 092 T03: `image_per_unit` opcional; configs antigas carregam sem ele e usam a estimativa
    /// padrão por qualidade.
    #[test]
    fn image_price_is_optional_with_a_default_estimate_per_quality() {
        let old: Config = toml::from_str("[pricing.\"m\"]\ninput_per_mtok = 1\noutput_per_mtok = 2\n").expect("antiga");
        let p = old.pricing["m"].clone();
        assert_eq!(p.image_per_unit, None);
        assert!(!toml::to_string(&old).expect("toml").contains("image_per_unit"), "ausente continua ausente");
        assert_eq!(p.image_price(Some("low")), (0.02, true));
        assert_eq!(p.image_price(Some("medium")), (0.06, true));
        assert_eq!(p.image_price(Some("high")), (0.25, true));
        assert_eq!(p.image_price(None), (0.06, true));
        let set: Config =
            toml::from_str("[pricing.\"m\"]\ninput_per_mtok = 1\noutput_per_mtok = 2\nimage_per_unit = 0.04\n").expect("nova");
        let q = set.pricing["m"].clone();
        assert_eq!(q.image_price(Some("high")), (0.04, false));
        let back: Config = toml::from_str(&toml::to_string(&set).expect("toml")).expect("reload");
        assert_eq!(back.pricing, set.pricing);
        assert!(!Pricing { image_per_unit: Some(-0.1), ..q.clone() }.is_valid());
        assert!(!Pricing { image_per_unit: Some(f64::INFINITY), ..q }.is_valid());
    }

    #[test]
    fn debug_redacts_api_key() {
        let conn = Connection {
            id: "c".into(),
            name: "n".into(),
            base_url: "https://x".into(),
            api_key: "sk-segredo-123".into(),
            wire_api: WireApi::Responses,
            kind: Default::default(),
            base_url_override: None,
        };
        let dbg = format!("{conn:?}");
        assert!(!dbg.contains("sk-segredo-123"), "vazou segredo: {dbg}");
        assert!(dbg.contains("***"));
        let cfg = Config {
            connections: vec![conn],
            ..Config::default()
        };
        assert!(!format!("{cfg:?}").contains("sk-segredo-123"));
    }

    #[test]
    fn wire_api_defaults_to_chat() {
        assert_eq!(WireApi::default(), WireApi::Chat);
        assert_eq!(WireApi::default().as_str(), "chat");
        assert_eq!(WireApi::Responses.as_str(), "responses");
    }

    #[test]
    fn connection_without_wire_api_loads_as_chat() {
        let conn: Connection = toml::from_str(
            r#"
id = "c"
name = "n"
base_url = "https://x"
api_key = "k"
"#,
        )
        .expect("conexão antiga");
        assert_eq!(conn.wire_api, WireApi::Chat);
    }

    #[test]
    fn wire_api_serializes_lowercase_and_round_trips() {
        for wire in [WireApi::Chat, WireApi::Responses] {
            let text = toml::to_string(&Connection {
                id: "c".into(),
                name: "n".into(),
                base_url: "https://x".into(),
                api_key: "k".into(),
                wire_api: wire,
                kind: Default::default(),
                base_url_override: None,
            })
            .expect("toml");
            assert!(text.contains(&format!("wire_api = \"{}\"", wire.as_str())), "{text}");
            let back: Connection = toml::from_str(&text).expect("reload");
            assert_eq!(back.wire_api, wire);
        }
    }
}

#[cfg(test)]
mod price_source_tests {
    use super::*;

    #[test]
    fn formato_antigo_sem_origem_e_manual_e_estimado_faz_round_trip() {
        let cfg: Config = toml::from_str(
            "[pricing.a]\ninput_per_mtok = 1\noutput_per_mtok = 2\n\n[pricing.b]\ninput_per_mtok = 1\noutput_per_mtok = 2\nsource = \"estimated\"\nestimated_at = \"2026-10-15\"\n",
        )
        .expect("toml");
        assert_eq!(cfg.pricing["a"].source, PriceSource::Manual);
        assert_eq!(cfg.pricing["b"].source, PriceSource::Estimated);
        let back: Config = toml::from_str(&toml::to_string(&cfg).expect("ser")).expect("de");
        assert_eq!(back.pricing, cfg.pricing);
    }

    #[test]
    fn architect_internet_defaults_and_overrides() {
        let cfg: Config = toml::from_str("").expect("vazio");
        assert_eq!(cfg.architect_internet.plan(), (5, 8, 10));
        assert_eq!(cfg.architect_internet.recovery(), (3, 5, 10));
        let cfg: Config = toml::from_str("[architect_internet]\nplan_pages = 2\nrecovery_searches = 0\n").expect("parcial");
        assert_eq!(cfg.architect_internet.plan(), (5, 2, 10));
        assert_eq!(cfg.architect_internet.recovery(), (0, 5, 10));
        let text = toml::to_string(&Config::default()).expect("ser");
        assert!(!text.contains("architect_internet"), "padrão não é gravado: {text}");
        let text = toml::to_string(&cfg).expect("ser");
        assert!(text.contains("plan_pages = 2"), "{text}");
    }

    #[test]
    fn max_steps_is_optional_round_trips_and_is_clamped_on_use() {
        // Config antiga, sem o campo: carrega e usa o padrão do papel.
        let cfg: Config = toml::from_str(
            "[agents.developer]\nconnection_id = \"c\"\nmodel = \"m\"\neffort = \"low\"\n",
        )
        .expect("config antiga");
        assert_eq!(cfg.agents.developer.max_steps, None);
        assert_eq!(cfg.agents.developer.max_steps_or(150), 150);
        let text = toml::to_string(&cfg).expect("ser");
        assert!(!text.contains("max_steps"), "None não é gravado: {text}");
        // Com o campo: o agente usa o valor e ele sobrevive à ida e volta.
        let cfg: Config = toml::from_str("[agents.reviewer]\nmax_steps = 60\n").expect("com campo");
        assert_eq!(cfg.agents.reviewer.max_steps_or(40), 60);
        let back: Config = toml::from_str(&toml::to_string(&cfg).expect("ser")).expect("de");
        assert_eq!(back.agents.reviewer.max_steps, Some(60));
        let fora = |n| AgentConfig { max_steps: Some(n), ..AgentConfig::default() };
        assert_eq!(fora(3).max_steps_or(150), 10);
        assert_eq!(fora(5000).max_steps_or(150), 1000);
    }

    #[test]
    fn server_lan_is_absent_until_chosen_and_round_trips() {
        let cfg: Config = toml::from_str("[server]\nport = 3000\n").expect("sem lan");
        assert_eq!(cfg.server.lan, None, "ausente = nunca escolhida");
        assert!(!toml::to_string(&cfg).expect("ser").contains("lan ="));
        for value in [true, false] {
            let cfg: Config = toml::from_str(&format!("[server]\nlan = {value}\n")).expect("com lan");
            assert_eq!(cfg.server.lan, Some(value));
            let back: Config = toml::from_str(&toml::to_string(&cfg).expect("ser")).expect("de");
            assert_eq!(back.server.lan, Some(value));
        }
    }
}
