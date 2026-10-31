//! Estado compartilhado entre os handlers do servidor.

use crate::i18n::{Lang, SharedLang};
use super::events::{ApiEvent, EventType};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{watch, RwLock};
#[cfg(test)]
use tokio::sync::Notify;

use super::app::AppManager;
use super::approvals::ApprovalRegistry;
use super::events::{EventBus, SecretSet};
use super::models_api::{ModelsCache, MODELS_TIMEOUT};
use super::report::REPORT_TIMEOUT;
use super::runs::{ConfiguredRunnerFactory, RunGuard, RunRegistry, RunnerFactory, SHUTDOWN_GRACE};
use crate::agents::chat::ChatBackend;
use crate::config::{Config, Project, WireApi};

/// Prefixo das reservas internas (remoção, configuração): nunca aparecem como execução.
pub const INTERNAL_RESERVATION_PREFIX: &str = "__";
/// Nome da reserva de execução usada durante a remoção do projeto.
pub const REMOVE_RESERVATION: &str = "__remove__";
/// Nome da reserva de execução usada durante a escrita de `settings.yaml`.
pub const SETTINGS_RESERVATION: &str = "__settings__";
/// Nome da reserva de execução usada durante a geração do relatório final da spec.
/// Nome da reserva de execução usada durante a partida do app de teste.
pub const APP_RESERVATION: &str = "__app__";
/// Nome da reserva de execução usada durante o levantamento da arquitetura.
pub const SURVEY_RESERVATION: &str = "__survey__";
pub const REPORT_RESERVATION: &str = "__report__";

/// Ponto de pausa de teste antes da reserva de projeto (leitura da configuração).
pub const PAUSE_BEFORE_RESERVE: &str = "reserve";
/// Ponto de pausa de teste antes da resolução de pedido de permissão.
pub const PAUSE_BEFORE_PERMISSION: &str = "permission";
/// Ponto de pausa de teste entre a busca do projeto e a reserva (sob o read lock).
pub const PAUSE_AFTER_LOOKUP: &str = "after_lookup";
/// Ponto de pausa de teste entre a checagem do projeto e a resolução (sob o read lock).
pub const PAUSE_AFTER_PERMISSION_LOOKUP: &str = "permission_lookup";

/// Falha ao reservar o projeto: não cadastrado, ou já ocupado por execução/chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveError {
    NotFound,
    Busy,
    /// O projeto está com o relatório final da spec sendo gerado (`__report__`).
    ReportInProgress,
}

/// Código (spec 115 T02) do 409 enquanto o relatório final da spec anterior é gerado.
pub const REPORT_BUSY_CODE: &str = "report_in_progress";

/// Pausa de teste: ao atingir `point`, sinaliza `reached` e espera `release`.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct PauseGate {
    pub point: &'static str,
    pub reached: Arc<Notify>,
    pub release: Arc<Notify>,
}

/// Backend do relatório final substituído (testes). `Debug` sem conteúdo.
#[derive(Clone)]
pub struct ReportBackend(Arc<dyn ChatBackend>);

impl std::fmt::Debug for ReportBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReportBackend")
    }
}

/// Fetcher do catálogo de preços. `Debug` sem conteúdo.
#[derive(Clone)]
pub struct PriceFetcherHandle(Arc<dyn super::price_catalog::PriceFetcher>);

impl std::fmt::Debug for PriceFetcherHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PriceFetcher")
    }
}

/// Validade da recusa de geração de imagens de um par (conexão, modelo) (spec 090): o usuário pode
/// implantar o modelo de imagem no provedor; depois disso o par volta a ser tentado.
pub const IMAGE_UNSUPPORTED_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// Relógio do registro de capacidade de imagens (injetável nos testes).
pub type ImageClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Capacidade de geração de imagens por (conexão, modelo), só em memória (some no reinício).
/// Desconhecido ou suportado = disponível; indisponível por [`IMAGE_UNSUPPORTED_TTL`].
#[derive(Clone)]
struct ImageCapability {
    clock: ImageClock,
    /// Instante da recusa de cada par.
    unsupported: Arc<Mutex<HashMap<(String, String), Instant>>>,
}

impl ImageCapability {
    fn new(clock: ImageClock) -> Self {
        Self { clock, unsupported: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn mark(&self, conn: &str, model: &str) {
        let now = (self.clock)();
        let mut map = self.unsupported.lock().unwrap_or_else(PoisonError::into_inner);
        map.insert((conn.to_string(), model.to_string()), now);
    }

    /// O par está marcado como indisponível e a marca ainda vale? Marca vencida é removida.
    fn rejected(&self, conn: &str, model: &str) -> bool {
        let now = (self.clock)();
        let mut map = self.unsupported.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (conn.to_string(), model.to_string());
        let Some(at) = map.get(&key).copied() else {
            return false;
        };
        if now.saturating_duration_since(at) < IMAGE_UNSUPPORTED_TTL {
            return true;
        }
        map.remove(&key);
        false
    }
}

/// `Debug` sem o relógio: só a quantidade de pares marcados.
impl std::fmt::Debug for ImageCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.unsupported.lock().unwrap_or_else(PoisonError::into_inner).len();
        write!(f, "ImageCapability({n} indisponível(is))")
    }
}

/// Teto de projetos com idioma lembrado (spec 102); além dele, o idioma vale só para a requisição.
const MAX_PROJECT_LANGS: usize = 4096;

/// Estado clonável (campos em `Arc`) injetado nos handlers e middlewares.
#[derive(Debug, Clone)]
pub struct AppState {
    config_path: Arc<PathBuf>,
    config: Arc<RwLock<Config>>,
    bound_port: u16,
    host_policy: super::security::HostPolicy,
    /// Faixa das portas de visualização configurada para o `--lan` (spec 097); `None` usa a
    /// padrão derivada da porta.
    view_ports: Option<crate::config::ViewPorts>,
    events: EventBus,
    approvals: ApprovalRegistry,
    shutdown: Arc<watch::Sender<bool>>,
    /// Projetos com um turno de chat em andamento (um por projeto). Sem I/O sob este lock.
    busy_chats: Arc<Mutex<HashSet<String>>>,
    /// Consenso do plano (spec 128 T02): mensagens do usuário e turnos internos por projeto.
    plan_consensus: Arc<super::plan_consensus::Runtime>,
    /// Turno do Arquiteto em andamento por projeto (texto do usuário já redigido).
    architect_turns: Arc<Mutex<std::collections::HashMap<String, ArchitectTurn>>>,
    /// Execuções de spec em andamento, no máximo uma por projeto.
    runs: RunRegistry,
    /// Apps de teste em execução, no máximo um por projeto.
    apps: AppManager,
    /// Fábrica dos runners (produção: ports reais; testes: fakes).
    runner_factory: Arc<dyn RunnerFactory>,
    /// Espera do shutdown pelo runner (cancelamento cooperativo) antes de abandoná-lo.
    shutdown_grace: Duration,
    /// Backend do relatório final no lugar do cliente configurado (testes); `None` em produção.
    report_backend: Option<ReportBackend>,
    /// Backend único de todos os papéis (chat, Desenvolvedor, Revisor) no lugar do cliente HTTP (testes).
    llm_backend: Option<ReportBackend>,
    /// Tempo total do turno do relatório final.
    report_timeout: Duration,
    /// Listagens de modelos em cache e locks por conexão.
    models: Arc<ModelsCache>,
    /// Timeout total da consulta de modelos (testes usam prazo curto).
    models_timeout: Duration,
    /// Diagnóstico de pré-requisitos (executor injetável).
    prereqs: super::prereqs::PrereqChecker,
    /// Fábrica dos Compose por projeto (spec 030; executor injetável).
    compose: super::envs::ComposeProvider,
    /// Obtém o catálogo de preços estimados (injetável; testes nunca usam a rede).
    price_fetcher: PriceFetcherHandle,
    /// Conexões cujo provedor rejeitou a busca nativa nesta execução do servidor (spec 076).
    web_search_rejected: Arc<Mutex<HashSet<String>>>,
    /// Capacidade de geração de imagens por (conexão, modelo) nesta execução (spec 090).
    image_capability: ImageCapability,
    /// Diretório de dados do Nants (estado da UI em `ui/`); `None` sem `XDG_DATA_HOME`/`HOME`.
    /// Os testes injetam um diretório temporário com [`Self::with_data_dir`].
    data_dir: Option<Arc<PathBuf>>,
    /// Serializa as gravações do estado da UI (conferência de `rev` + escrita).
    ui_state_lock: Arc<tokio::sync::Mutex<()>>,
    /// Idioma ativo da interface por projeto (spec 102), atualizado a cada turno pelo cabeçalho
    /// `X-Nants-Lang` (ver `server::lang`). Só em memória.
    langs: Arc<Mutex<std::collections::HashMap<String, SharedLang>>>,
    /// Último idioma informado em qualquer requisição: o inicial de um projeto ainda sem idioma.
    last_lang: SharedLang,
    /// Conferência do Windows para Configurações > Acesso pela rede (spec 111; injetável).
    net_probe: super::lan::NetProbe,
    /// Modo de rede desta execução (spec 111).
    network_run: super::lan::NetworkRun,
    /// Dispositivos autorizados na rede local (spec 112), em `devices.json` da pasta de dados.
    devices: Arc<super::devices::DeviceStore>,
    /// O endereço de escuta não é loopback (spec 112): exige dispositivo como o modo rede.
    exposed: bool,
    /// Login pelo plano ChatGPT (spec 116): tentativas pendentes, JWKS e renovações.
    chatgpt: Arc<super::chatgpt::ChatgptAuth>,
    /// Fila de extração da base de conhecimento (spec 134/T02). Parada nos testes, que ligam a
    /// sua com [`Self::with_kb_queue`].
    kb_queue: super::kb_queue::KbQueue,
    /// IPv4 de rede do Windows conferidos há pouco (spec 112), com o instante da conferência.
    windows_ips: Arc<tokio::sync::Mutex<Option<(Instant, Option<Vec<std::net::Ipv4Addr>>)>>>,
    /// Nanty (spec 138 T01): estado e arquivos em `<pasta de dados>/nanty`. O laço de fundo não
    /// nasce aqui: o `main.rs` o liga com `nanty::start`.
    nanty: Arc<super::nanty::Nanty>,
    /// Endereço da API do Telegram (spec 138 T06). Fixo em produção; só os testes trocam, pelo
    /// [`Self::with_telegram_api`] (um servidor falso local). Nunca vem da configuração.
    telegram_api: Arc<str>,
    /// Pausas de teste (vazio em produção).
    #[cfg(test)]
    gates: Arc<Mutex<Vec<PauseGate>>>,
}

impl AppState {
    /// `config_path` é onde a configuração é persistida; `port` é a porta
    /// efetivamente ligada, usada na validação de `Host` e `Origin`.
    pub fn new(config_path: PathBuf, config: Config, port: u16) -> Self {
        let events = EventBus::new();
        #[cfg(not(test))]
        let events_for_kb = events.clone();
        #[cfg(not(test))]
        let data_dir: Option<Arc<PathBuf>> = crate::config::data_dir().map(Arc::new);
        // Testes nunca escrevem na pasta de dados real do usuário.
        #[cfg(test)]
        let data_dir: Option<Arc<PathBuf>> =
            Some(Arc::new(std::env::temp_dir().join(format!("nants-test-data-{}", std::process::id()))));
        let devices = Arc::new(super::devices::DeviceStore::new(
            data_dir.as_deref().map(|d| d.join(super::devices::DEVICES_FILE)),
        ));
        let nanty = Arc::new(super::nanty::Nanty::new(
            data_dir.as_deref().map(|d| d.join(super::nanty::NANTY_DIR)),
            events.clone(),
            super::nanty::clock::system_clock(),
        ));
        Self {
            apps: AppManager::new(events.clone()),
            config_path: Arc::new(config_path),
            config: Arc::new(RwLock::new(config)),
            bound_port: port,
            host_policy: super::security::HostPolicy::Loopback,
            view_ports: None,
            events,
            approvals: ApprovalRegistry::new(),
            shutdown: Arc::new(watch::channel(false).0),
            busy_chats: Arc::new(Mutex::new(HashSet::new())),
            plan_consensus: Arc::new(super::plan_consensus::Runtime::default()),
            architect_turns: Arc::new(Mutex::new(std::collections::HashMap::new())),
            runs: RunRegistry::new(),
            runner_factory: Arc::new(ConfiguredRunnerFactory),
            shutdown_grace: SHUTDOWN_GRACE,
            report_backend: None,
            llm_backend: None,
            report_timeout: REPORT_TIMEOUT,
            models: Arc::new(ModelsCache::default()),
            models_timeout: MODELS_TIMEOUT,
            #[cfg(not(test))]
            prereqs: super::prereqs::PrereqChecker::real(),
            // Testes: Docker "disponível" por padrão (Docker é obrigatório, spec 074); sem comando real.
            #[cfg(test)]
            prereqs: super::prereqs::PrereqChecker::new(Arc::new(|_: &str, _: &[&str]| {
                Some(super::prereqs::CmdOutput { success: true, stdout: "ok".into(), stderr: String::new() })
            })),
            compose: super::envs::ComposeProvider::default(),
            #[cfg(not(test))]
            price_fetcher: PriceFetcherHandle(Arc::new(super::price_catalog::HttpPriceFetcher)),
            #[cfg(test)]
            price_fetcher: PriceFetcherHandle(Arc::new(super::price_catalog::StaticPriceFetcher(None))),
            web_search_rejected: Arc::new(Mutex::new(HashSet::new())),
            image_capability: ImageCapability::new(Arc::new(Instant::now)),
            data_dir,
            ui_state_lock: Arc::new(tokio::sync::Mutex::new(())),
            langs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            last_lang: SharedLang::default(),
            #[cfg(not(test))]
            net_probe: super::lan::NetProbe::real(),
            // Testes nunca consultam o Windows de verdade: fora do WSL e sem programas.
            #[cfg(test)]
            net_probe: super::lan::NetProbe {
                tools: super::portproxy::WinTools { run: Arc::new(|_: &str, _: &[&str]| None), exists: Arc::new(|_: &str| false) },
                wsl: false,
                local_ips: Some(Vec::new()),
            },
            network_run: super::lan::NetworkRun { lan: false, source: super::lan::ModeSource::Default, requested_port: port },
            devices,
            exposed: false,
            chatgpt: Arc::new(super::chatgpt::ChatgptAuth::default()),
            #[cfg(not(test))]
            kb_queue: super::kb_queue::KbQueue::new(
                events_for_kb,
                crate::harness::web::WebClient::new(),
                super::kb_queue::EXTRACT_TIMEOUT,
            ),
            #[cfg(test)]
            kb_queue: super::kb_queue::KbQueue::idle(),
            windows_ips: Arc::new(tokio::sync::Mutex::new(None)),
            nanty,
            #[cfg(not(test))]
            telegram_api: Arc::from(super::nanty::telegram::TELEGRAM_API),
            // Testes nunca falam com o Telegram real: sem o servidor falso, um endereço morto.
            #[cfg(test)]
            telegram_api: Arc::from("http://127.0.0.1:9"),
            #[cfg(test)]
            gates: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Conferência do Windows usada por `GET /api/config/network` (testes injetam uma falsa).
    pub fn with_net_probe(mut self, probe: super::lan::NetProbe) -> Self {
        self.net_probe = probe;
        self
    }

    pub fn net_probe(&self) -> &super::lan::NetProbe {
        &self.net_probe
    }

    /// Modo de rede desta execução (spec 111), decidido ao subir.
    pub fn with_network_run(mut self, run: super::lan::NetworkRun) -> Self {
        self.network_run = run;
        self
    }

    pub fn network_run(&self) -> super::lan::NetworkRun {
        self.network_run
    }

    /// Modo rede (spec 112): o servidor aceita conexões de fora do loopback. Vale a escuta fora do
    /// loopback, a política de `Host` da rede ou o modo desta execução (qualquer um liga a exigência
    /// de dispositivo).
    pub fn lan_mode(&self) -> bool {
        self.exposed || self.host_policy == super::security::HostPolicy::Lan || self.network_run.lan
    }

    /// Escuta fora do loopback (por exemplo `--host` com `--allow-remote`): exige dispositivo.
    pub fn with_exposed(mut self, exposed: bool) -> Self {
        self.exposed = exposed;
        self
    }

    /// IPv4 de rede do Windows (spec 112), só no WSL, com cache de [`super::lan::WINDOWS_IPS_TTL`].
    /// `None`: fora do WSL ou não deu para conferir.
    pub async fn windows_ips(&self) -> Option<Vec<std::net::Ipv4Addr>> {
        if !self.net_probe.wsl {
            return None;
        }
        let mut cache = self.windows_ips.lock().await;
        if let Some((at, ips)) = cache.as_ref() {
            if at.elapsed() < super::lan::WINDOWS_IPS_TTL {
                return ips.clone();
            }
        }
        let tools = self.net_probe.tools.clone();
        let ips = tokio::time::timeout(
            super::lan::PROBE_DEADLINE,
            tokio::task::spawn_blocking(move || super::lan::windows_ips_with(&tools, true)),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
        *cache = Some((Instant::now(), ips.clone()));
        ips
    }

    /// Login pelo plano ChatGPT (spec 116).
    pub fn chatgpt(&self) -> &Arc<super::chatgpt::ChatgptAuth> {
        &self.chatgpt
    }

    /// Substitui o login pelo plano ChatGPT (testes: servidor de autenticação simulado e relógio).
    pub fn with_chatgpt(mut self, auth: super::chatgpt::ChatgptAuth) -> Self {
        self.chatgpt = Arc::new(auth);
        self
    }

    /// Dispositivos autorizados (spec 112).
    pub fn devices(&self) -> &Arc<super::devices::DeviceStore> {
        &self.devices
    }

    /// Relógio dos dispositivos (testes), mantendo o arquivo atual.
    #[cfg(test)]
    pub fn with_device_clock(mut self, clock: super::devices::DeviceClock) -> Self {
        let path = self.devices.path().map(Path::to_path_buf);
        self.devices = Arc::new(super::devices::DeviceStore::with_clock(path, clock));
        self
    }

    /// Idioma ativo do projeto (spec 102), compartilhado: quem guarda o handle (agentes de uma
    /// execução, turno do Arquiteto) vê a troca feita depois. Sem idioma informado ainda, o último
    /// informado em qualquer requisição; sem nenhum, pt-BR.
    pub fn project_lang(&self, project_id: &str) -> SharedLang {
        let mut langs = self.langs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(lang) = langs.get(project_id) {
            return lang.clone();
        }
        let lang = SharedLang::new(self.last_lang.get());
        if langs.len() < MAX_PROJECT_LANGS {
            langs.insert(project_id.to_string(), lang.clone());
        }
        lang
    }

    /// Registra o idioma informado pela interface (global e, com projeto, o do projeto).
    pub fn set_lang(&self, project_id: Option<&str>, lang: Lang) {
        self.last_lang.set(lang);
        if let Some(id) = project_id {
            self.project_lang(id).set(lang);
        }
    }

    /// Substitui o diretório de dados do Nants (testes: diretório temporário próprio).
    pub fn with_data_dir(mut self, dir: PathBuf) -> Self {
        self.devices = Arc::new(super::devices::DeviceStore::new(Some(dir.join(super::devices::DEVICES_FILE))));
        let probe = self.nanty.probe();
        self.nanty = Arc::new(super::nanty::Nanty::new(
            Some(dir.join(super::nanty::NANTY_DIR)),
            self.events.clone(),
            self.nanty.clock(),
        ));
        self.nanty.set_probe(probe);
        self.data_dir = Some(Arc::new(dir));
        self
    }

    /// O Nanty deste processo (spec 138).
    pub fn nanty(&self) -> &Arc<super::nanty::Nanty> {
        &self.nanty
    }

    /// Troca o relógio do Nanty (testes: hora local fixa). Chame depois de [`Self::with_data_dir`].
    pub fn with_nanty_clock(mut self, clock: super::nanty::clock::NantyClock) -> Self {
        let dir = self.data_dir().map(|d| d.join(super::nanty::NANTY_DIR));
        let probe = self.nanty.probe();
        self.nanty = Arc::new(super::nanty::Nanty::new(dir, self.events.clone(), clock));
        self.nanty.set_probe(probe);
        self
    }

    /// Troca a sonda do disco e do Docker do Nanty (testes).
    pub fn with_nanty_probe(self, probe: Arc<dyn super::nanty::observe::NantyProbe>) -> Self {
        self.nanty.set_probe(probe);
        self
    }

    /// Troca o endereço da API do Telegram (testes: o servidor falso local). Nunca pela
    /// configuração (spec 138 T06, C-09).
    pub fn with_telegram_api(mut self, base: &str) -> Self {
        self.telegram_api = Arc::from(base.trim_end_matches('/'));
        self
    }

    /// Endereço da API do Telegram.
    pub fn telegram_api(&self) -> &str {
        &self.telegram_api
    }

    /// Diretório de dados do Nants, se conhecido.
    pub fn data_dir(&self) -> Option<&Path> {
        self.data_dir.as_deref().map(PathBuf::as_path)
    }

    /// Lock das gravações do estado da UI.
    pub fn ui_state_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.ui_state_lock
    }

    /// O provedor desta conexão já rejeitou a busca nativa nesta execução do servidor?
    pub fn web_search_rejected(&self, connection_id: &str) -> bool {
        self.web_search_rejected.lock().unwrap_or_else(PoisonError::into_inner).contains(connection_id)
    }

    /// Registra a rejeição da busca nativa pela conexão (vale até reiniciar o servidor).
    pub fn mark_web_search_rejected(&self, connection_id: &str) {
        self.web_search_rejected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(connection_id.to_string());
    }

    /// A conexão do Arquiteto oferece busca nativa: `wire_api = responses`, a Anthropic (spec 132
    /// T05) ou o Gemini (`google_search`, T07), e sem rejeição registrada.
    pub async fn web_search_available(&self) -> bool {
        let connection_id = {
            let cfg = self.config.read().await;
            let agent = &cfg.agents.architect;
            let supports = cfg.connections.iter().any(|c| {
                c.id == agent.connection_id
                    && (matches!(c.wire_api, crate::config::WireApi::Responses)
                        || c.kind.is_native())
            });
            supports.then(|| agent.connection_id.clone())
        };
        connection_id.is_some_and(|id| !self.web_search_rejected(&id))
    }

    /// Registra a recusa da geração de imagens pelo par (conexão, modelo): vale por
    /// [`IMAGE_UNSUPPORTED_TTL`] e some no reinício do servidor.
    pub fn mark_image_unsupported(&self, connection_id: &str, model: &str) {
        self.image_capability.mark(connection_id, model);
    }

    /// O par gera imagens: `wire_api = responses` e sem recusa válida registrada.
    pub fn image_generation_available(&self, connection_id: &str, model: &str, wire: WireApi) -> bool {
        matches!(wire, WireApi::Responses) && !self.image_capability.rejected(connection_id, model)
    }

    /// O par do papel Desenvolvedor gera imagens ([`Self::image_generation_available`]); conexão
    /// inexistente = não. Pelo plano ChatGPT, também não (spec 116 T02: a ferramenta hospedada de
    /// imagem não funciona por ali).
    pub async fn developer_image_generation_available(&self) -> bool {
        let pair = {
            let cfg = self.config.read().await;
            let agent = &cfg.agents.developer;
            let wire = cfg
                .connections
                .iter()
                .find(|c| c.id == agent.connection_id && !c.is_chatgpt_plan())
                .map(|c| c.wire_api);
            wire.map(|w| (agent.connection_id.clone(), agent.model.clone(), w))
        };
        pair.is_some_and(|(conn, model, wire)| self.image_generation_available(&conn, &model, wire))
    }

    /// Substitui o relógio do registro de capacidade de imagens (testes).
    #[cfg(test)]
    pub fn with_image_clock(mut self, clock: ImageClock) -> Self {
        self.image_capability = ImageCapability::new(clock);
        self
    }

    /// Substitui o fetcher do catálogo de preços (testes).
    pub fn with_price_fetcher(mut self, fetcher: Arc<dyn super::price_catalog::PriceFetcher>) -> Self {
        self.price_fetcher = PriceFetcherHandle(fetcher);
        self
    }

    /// Fetcher do catálogo de preços.
    pub fn price_fetcher(&self) -> &dyn super::price_catalog::PriceFetcher {
        self.price_fetcher.0.as_ref()
    }

    /// Substitui o diagnóstico de pré-requisitos (testes).
    pub fn with_prereqs(mut self, p: super::prereqs::PrereqChecker) -> Self {
        self.prereqs = p;
        self
    }

    /// `true` se o projeto está ativo: app (ou start em andamento), run/relatório ou turno de chat.
    pub fn project_active(&self, project_id: &str) -> bool {
        self.apps.is_busy(project_id)
            || self.runs.active(project_id).is_some()
            || self.busy_chats.lock().unwrap_or_else(|e| e.into_inner()).contains(project_id)
    }

    /// Verificação estrita de atividade (spec 138 T03, C-16), para a limpeza do Nanty: qualquer vaga
    /// no registro de execuções (inclusive `__app__`, `__survey__` e `__report__`), app vivo, em
    /// partida ou com o container encerrado ainda guardado, comando no Console (ou o `-run` que ele
    /// levantou) e turno de chat.
    pub fn project_busy_strict(&self, project_id: &str) -> bool {
        self.runs.holds(project_id)
            || self.apps.is_busy(project_id)
            || self.apps.is_docker_running(project_id)
            || self.apps.console().is_busy(project_id)
            || self.apps.console().has_raised(project_id)
            || self.busy_chats.lock().unwrap_or_else(|e| e.into_inner()).contains(project_id)
    }

    /// Substitui a fábrica de Compose (testes).
    pub fn with_compose_provider(mut self, provider: super::envs::ComposeProvider) -> Self {
        self.compose = provider;
        self
    }

    /// Fábrica dos Compose por projeto.
    pub fn compose(&self) -> &super::envs::ComposeProvider {
        &self.compose
    }

    /// Diagnóstico de pré-requisitos.
    pub fn prereqs(&self) -> &super::prereqs::PrereqChecker {
        &self.prereqs
    }

    /// Altera o timeout da consulta de modelos (padrão: `MODELS_TIMEOUT`).
    pub fn with_models_timeout(mut self, timeout: Duration) -> Self {
        self.models_timeout = timeout;
        self
    }

    /// Timeout total da consulta de modelos.
    pub fn models_timeout(&self) -> Duration {
        self.models_timeout
    }

    /// Cache das listagens de modelos.
    pub fn models_cache(&self) -> &ModelsCache {
        &self.models
    }

    /// Substitui a fábrica de runners (usado em testes para não chamar LLM).
    pub fn with_runner_factory(mut self, factory: Arc<dyn RunnerFactory>) -> Self {
        self.runner_factory = factory;
        self
    }

    /// Altera o prazo de espera do shutdown pelo runner (testes usam prazo curto).
    pub fn with_shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }

    /// Prazo que o shutdown concede ao runner antes de abandoná-lo.
    pub fn shutdown_grace(&self) -> Duration {
        self.shutdown_grace
    }

    /// Substitui o backend do relatório final (usado em testes para não chamar LLM).
    pub fn with_report_backend(mut self, backend: Arc<dyn ChatBackend>) -> Self {
        self.report_backend = Some(ReportBackend(backend));
        self
    }

    /// Substitui o backend de TODOS os papéis (chat do Arquiteto, Desenvolvedor, Revisor, relatório
    /// e arquitetura) por um fake que decide a resposta pelo prompt de sistema (testes ponta a ponta).
    pub fn with_llm_backend(mut self, backend: Arc<dyn ChatBackend>) -> Self {
        self.report_backend = Some(ReportBackend(Arc::clone(&backend)));
        self.llm_backend = Some(ReportBackend(backend));
        self
    }

    /// Backend substituto de todos os papéis, se houver.
    pub fn llm_backend(&self) -> Option<Arc<dyn ChatBackend>> {
        self.llm_backend.as_ref().map(|b| Arc::clone(&b.0))
    }

    /// Backend substituto do relatório final, se houver.
    pub fn report_backend(&self) -> Option<Arc<dyn ChatBackend>> {
        self.report_backend.as_ref().map(|b| Arc::clone(&b.0))
    }

    /// Altera o tempo total do turno do relatório final (padrão: `REPORT_TIMEOUT`).
    pub fn with_report_timeout(mut self, timeout: Duration) -> Self {
        self.report_timeout = timeout;
        self
    }

    /// Tempo total do turno do relatório final.
    pub fn report_timeout(&self) -> Duration {
        self.report_timeout
    }

    /// Execuções de spec do processo.
    pub fn runs(&self) -> &RunRegistry {
        &self.runs
    }

    /// Apps de teste do processo.
    pub fn apps(&self) -> &AppManager {
        &self.apps
    }

    /// Janela de observação pós-`up` do `-run` (testes usam zero).
    pub fn with_app_observe_window(mut self, window: Duration) -> Self {
        self.apps = self.apps.with_observe_window(window);
        self
    }

    /// Timeout da sonda `embeddable` do app (testes usam prazo curto).
    pub fn with_app_probe_timeout(mut self, timeout: Duration) -> Self {
        self.apps = self.apps.with_probe_timeout(timeout);
        self
    }

    /// Intervalo da sondagem leve do modo do app (spec 130 T02; testes usam zero ou prazo curto).
    pub fn with_app_watch_interval(mut self, interval: Duration) -> Self {
        self.apps = self.apps.with_watch_interval(interval);
        self
    }

    /// Limites do Console do projeto (spec 130 T03; testes usam prazos curtos).
    pub fn with_console_limits(mut self, limits: super::console::ConsoleLimits) -> Self {
        self.apps = self.apps.with_console_limits(limits);
        self
    }

    /// Fábrica usada para montar o runner de cada aprovação.
    pub fn runner_factory(&self) -> &Arc<dyn RunnerFactory> {
        &self.runner_factory
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn config(&self) -> &RwLock<Config> {
        &self.config
    }

    pub fn host_policy(&self) -> super::security::HostPolicy {
        self.host_policy
    }

    /// Define a política de `Host` (modo `--lan`).
    pub fn with_host_policy(mut self, policy: super::security::HostPolicy) -> Self {
        self.host_policy = policy;
        self
    }

    pub fn bound_port(&self) -> u16 {
        self.bound_port
    }

    /// Faixa das portas de visualização já validada na inicialização (spec 097).
    pub fn with_view_ports(mut self, range: crate::config::ViewPorts) -> Self {
        self.view_ports = Some(range);
        self
    }

    /// Faixa das portas de visualização em uso: só no modo `--lan`; sem faixa configurada, a
    /// padrão derivada da porta efetiva.
    pub fn lan_view_ports(&self) -> Option<crate::config::ViewPorts> {
        if self.host_policy != super::security::HostPolicy::Lan {
            return None;
        }
        self.view_ports.or_else(|| crate::config::ViewPorts::default_for(self.bound_port))
    }

    /// Fila de extração da base de conhecimento.
    pub fn kb_queue(&self) -> &super::kb_queue::KbQueue {
        &self.kb_queue
    }

    /// Troca a fila de extração (testes: cliente web com loopback e prazo curto).
    pub fn with_kb_queue(mut self, queue: super::kb_queue::KbQueue) -> Self {
        self.kb_queue = queue;
        self
    }

    /// Barramento de eventos do processo (SSE e publicação).
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Pedidos de permissão (HITL) pendentes do processo.
    pub fn approvals(&self) -> &ApprovalRegistry {
        &self.approvals
    }

    /// Conjunto de segredos da configuração atual (reconstruído a cada chamada, sem cache).
    pub async fn secret_set(&self) -> Arc<SecretSet> {
        let cfg = self.config.read().await;
        Arc::new(SecretSet::from_config(&cfg))
    }

    /// Turno do Arquiteto em andamento no projeto, se houver.
    pub fn architect_turn(&self, project_id: &str) -> Option<ArchitectTurn> {
        let map = self.architect_turns.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(project_id).cloned()
    }

    /// Registra o turno em andamento e devolve a guarda que o remove e publica
    /// `chat_turn_ended` em qualquer caminho de saída (inclusive abort).
    pub fn begin_architect_turn(&self, project_id: &str, turn: ArchitectTurn, publish_text: bool) -> TurnScope {
        let mut payload = serde_json::json!({
            "project_id": project_id,
            "turn_id": turn.turn_id,
            "started_at": turn.started_at,
        });
        if publish_text {
            payload["user_text"] = serde_json::json!(turn.user_text);
        }
        self.architect_turns
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(project_id.to_string(), turn.clone());
        self.events.publish(ApiEvent::new(EventType::ChatTurnStarted, Some(project_id.to_string()), payload));
        TurnScope {
            turns: Arc::clone(&self.architect_turns),
            events: self.events.clone(),
            project_id: project_id.to_string(),
            turn_id: turn.turn_id,
            ok: false,
            reservation: None,
        }
    }

    /// Estado do consenso do plano por projeto (spec 128 T02).
    pub fn plan_consensus(&self) -> &Arc<super::plan_consensus::Runtime> {
        &self.plan_consensus
    }

    /// Reserva o turno de chat do projeto. `None` se já há um turno em andamento.
    /// A reserva é liberada ao soltar o [`ChatGuard`].
    pub fn try_begin_chat(&self, project_id: &str) -> Option<ChatGuard> {
        let mut busy = self
            .busy_chats
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !busy.insert(project_id.to_string()) {
            return None;
        }
        Some(ChatGuard {
            busy: Arc::clone(&self.busy_chats),
            project_id: project_id.to_string(),
        })
    }

    /// Reserva execução de `spec_id` e devolve o projeto. Busca e reserva ocorrem sob o mesmo
    /// read lock da configuração: uma remoção já commitada vira `NotFound`; uma reserva obtida
    /// bloqueia a remoção (que pede o write lock). Sem `await` com o lock de runs/chat.
    pub async fn reserve_run(
        &self,
        project_id: &str,
        spec_id: &str,
    ) -> Result<(Project, RunGuard), ReserveError> {
        self.pause(PAUSE_BEFORE_RESERVE).await;
        let cfg = self.config.read().await;
        let project = find_in(&cfg, project_id)?;
        self.pause(PAUSE_AFTER_LOOKUP).await;
        let guard = self
            .runs
            .try_start(project_id, spec_id)
            .map_err(|_| self.busy_error(project_id))?;
        Ok((project, guard))
    }

    /// Motivo da recusa quando a reserva do projeto já existe: relatório em andamento ou outra.
    pub fn busy_error(&self, project_id: &str) -> ReserveError {
        if self.runs.is_report(project_id) {
            ReserveError::ReportInProgress
        } else {
            ReserveError::Busy
        }
    }

    /// Reserva o turno de chat e devolve o projeto, sob o mesmo read lock de [`Self::reserve_run`].
    pub async fn reserve_chat(&self, project_id: &str) -> Result<(Project, ChatGuard), ReserveError> {
        self.pause(PAUSE_BEFORE_RESERVE).await;
        let cfg = self.config.read().await;
        let project = find_in(&cfg, project_id)?;
        self.pause(PAUSE_AFTER_LOOKUP).await;
        if self.runs.is_report(project_id) {
            return Err(ReserveError::ReportInProgress);
        }
        let guard = self.try_begin_chat(project_id).ok_or(ReserveError::Busy)?;
        Ok((project, guard))
    }

    /// Espera de teste num ponto nomeado; no-op em produção.
    pub async fn pause(&self, point: &'static str) {
        #[cfg(test)]
        {
            let gate = {
                let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
                gates
                    .iter()
                    .position(|g| g.point == point)
                    .map(|i| gates.remove(i))
            };
            if let Some(gate) = gate {
                gate.reached.notify_one();
                gate.release.notified().await;
            }
        }
        #[cfg(not(test))]
        {
            let _ = point;
        }
    }

    /// Instala uma pausa de teste (consumida na primeira passagem pelo ponto).
    #[cfg(test)]
    pub fn install_gate(&self, gate: PauseGate) {
        self.gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(gate);
    }

    /// Receptor que é notificado quando o shutdown do servidor for acionado.
    pub fn shutdown_signal(&self) -> watch::Receiver<bool> {
        self.shutdown.subscribe()
    }

    /// Aciona o shutdown: streams SSE abertos terminam. Idempotente.
    pub fn trigger_shutdown(&self) {
        self.shutdown.send_replace(true);
    }
}

fn find_in(cfg: &Config, project_id: &str) -> Result<Project, ReserveError> {
    cfg.projects
        .iter()
        .find(|p| p.id == project_id)
        .cloned()
        .ok_or(ReserveError::NotFound)
}

/// Reserva de turno de chat por projeto; libera a vaga ao ser descartada.
#[derive(Debug)]
pub struct ChatGuard {
    busy: Arc<Mutex<HashSet<String>>>,
    project_id: String,
}

impl Drop for ChatGuard {
    fn drop(&mut self) {
        let mut busy = self.busy.lock().unwrap_or_else(PoisonError::into_inner);
        busy.remove(&self.project_id);
    }
}

/// Turno do Arquiteto visível a qualquer aba (texto do usuário já redigido).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArchitectTurn {
    pub turn_id: String,
    pub user_text: String,
    pub started_at: u64,
}

/// Guarda do turno: ao ser solta remove o estado e publica `chat_turn_ended`.
#[derive(Debug)]
pub struct TurnScope {
    turns: Arc<Mutex<std::collections::HashMap<String, ArchitectTurn>>>,
    events: EventBus,
    project_id: String,
    turn_id: String,
    ok: bool,
    reservation: Option<ChatGuard>,
}

impl TurnScope {
    /// Entrega a reserva do chat à guarda: ela é liberada antes de publicar `chat_turn_ended`.
    pub fn hold(&mut self, reservation: ChatGuard) {
        self.reservation = Some(reservation);
    }

    pub fn succeed(&mut self) {
        self.ok = true;
    }
}

impl Drop for TurnScope {
    fn drop(&mut self) {
        self.turns.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.project_id);
        drop(self.reservation.take());
        self.events.publish(ApiEvent::new(
            EventType::ChatTurnEnded,
            Some(self.project_id.clone()),
            serde_json::json!({"project_id": self.project_id, "turn_id": self.turn_id, "ok": self.ok}),
        ));
    }
}

#[cfg(test)]
mod image_capability_tests {
    use super::*;

    /// Relógio manual: o instante base mais o deslocamento atual.
    fn manual_clock() -> (ImageClock, Arc<Mutex<Duration>>) {
        let base = Instant::now();
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let o = Arc::clone(&offset);
        let clock: ImageClock = Arc::new(move || base + *o.lock().unwrap_or_else(PoisonError::into_inner));
        (clock, offset)
    }

    fn state(clock: ImageClock) -> AppState {
        AppState::new(PathBuf::from("/tmp/nants-img-cap.toml"), Config::default(), 0).with_image_clock(clock)
    }

    #[test]
    fn unsupported_pair_expires_after_six_hours() {
        let (clock, offset) = manual_clock();
        let st = state(clock);
        assert!(st.image_generation_available("c", "m", WireApi::Responses), "desconhecido = disponível");
        assert!(!st.image_generation_available("c", "m", WireApi::Chat), "chat nunca");
        st.mark_image_unsupported("c", "m");
        assert!(!st.image_generation_available("c", "m", WireApi::Responses));
        // Outro modelo da mesma conexão e outra conexão do mesmo modelo seguem disponíveis.
        assert!(st.image_generation_available("c", "m2", WireApi::Responses));
        assert!(st.image_generation_available("c2", "m", WireApi::Responses));
        *offset.lock().expect("lock") = IMAGE_UNSUPPORTED_TTL - Duration::from_secs(1);
        assert!(!st.image_generation_available("c", "m", WireApi::Responses), "ainda dentro das 6 h");
        *offset.lock().expect("lock") = IMAGE_UNSUPPORTED_TTL;
        assert!(st.image_generation_available("c", "m", WireApi::Responses), "venceu");
        // Nova recusa recomeça a contagem a partir de agora.
        st.mark_image_unsupported("c", "m");
        *offset.lock().expect("lock") = IMAGE_UNSUPPORTED_TTL + Duration::from_secs(60);
        assert!(!st.image_generation_available("c", "m", WireApi::Responses));
    }

    #[test]
    fn registry_is_in_memory_only() {
        let (clock, _) = manual_clock();
        let st = state(Arc::clone(&clock));
        st.mark_image_unsupported("c", "m");
        assert!(!st.clone().image_generation_available("c", "m", WireApi::Responses), "clones compartilham");
        assert!(state(clock).image_generation_available("c", "m", WireApi::Responses), "reinício limpa");
        assert!(!format!("{st:?}").contains("\"m\""), "Debug só conta os pares");
    }
}
