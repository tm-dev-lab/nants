//! Gerador de imagens de produção (spec 090 T02): o `LlmClient` da conexão do Desenvolvedor e o
//! modelo dele, pela ferramenta hospedada `image_generation` (T01).
//!
//! - Disponibilidade: o registro de capacidade do `AppState` para o par (conexão, modelo), consultado
//!   a cada sessão do Desenvolvedor.
//! - Recusa: marca o par como indisponível e publica uma única vez por execução (este gerador vive
//!   uma execução) a mensagem de sistema [`IMAGE_UNAVAILABLE_NOTICE`].
//! - Falha final (spec 091): linha no stderr (também na recusa) e mensagem de sistema com o motivo
//!   fixo e o caminho, até [`MAX_FAILURE_NOTICES`] por execução.
//! - Freio de custo (spec 094 T01): o gerador vive uma execução e serve um par (conexão, modelo),
//!   então guarda o [`ChargedBrake`] da execução. Ao suspender, linha no stderr e mensagem de
//!   sistema única ([`image_suspended_notice`]), SEMPRE publicada, fora do teto da 091.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::state::AppState;
use crate::agents::llm_client::{
    log_image_failure, log_image_suspended, GeneratedImage, ImageOptions, LlmClient, ProviderFailure, IMAGE_UNSUPPORTED,
};
use crate::agents::runner::{EventSink, RunnerEvent};
use crate::agents::LlmError;
use crate::config::{Config, WireApi};
use crate::harness::tools::{image_suspended_notice, ChargedBrake, ImageGenerator, IMAGE_UNAVAILABLE_NOTICE};

/// Códigos (spec 102) das mensagens de sistema de imagem; a interface as mostra pelo `code`.
pub const CODE_IMAGE_UNAVAILABLE: &str = "image.unavailable";
/// Falha final da geração (`params.path`, `params.provider` e, quando houver, `params.kind` e
/// `params.provider_code`).
pub const CODE_IMAGE_FAILED: &str = "image.failed";
/// Geração suspensa pelo freio de custo (`params.count`: falhas cobradas).
pub const CODE_IMAGE_SUSPENDED: &str = "image.suspended";
/// Papel registrado no log das falhas de imagem (só o Desenvolvedor gera).
const IMAGE_ROLE: &str = "developer";
/// Teto de mensagens de sistema de falha de imagem por execução; depois, só o log.
pub const MAX_FAILURE_NOTICES: usize = 3;

/// Mensagem de sistema da falha final da geração de `path` (spec 091): só o motivo fixo.
pub fn image_failure_notice(path: &str, failure: &ProviderFailure) -> String {
    format!(
        "Geração de imagem falhou ({}): {path}. O Developer seguirá com SVG/CSS se necessário.",
        failure.describe()
    )
}

/// Gerador sobre a conexão e o modelo do Desenvolvedor.
pub struct LlmImageGenerator {
    client: LlmClient,
    connection_id: String,
    model: String,
    wire: WireApi,
    state: AppState,
    notice: Option<Arc<dyn EventSink>>,
    warned: AtomicBool,
    /// Mensagens de falha já publicadas nesta execução.
    failures: AtomicUsize,
    /// Freio de custo da execução para o par (spec 094 T01).
    brake: ChargedBrake,
}

impl LlmImageGenerator {
    /// Gerador do par do Desenvolvedor na configuração; `None` sem conexão, sem modelo ou com cliente
    /// inválido.
    pub fn for_developer(cfg: &Config, state: &AppState) -> Option<Self> {
        let agent = &cfg.agents.developer;
        let connection = cfg.connections.iter().find(|c| c.id == agent.connection_id)?;
        // Spec 116 T02: pelo plano ChatGPT a ferramenta hospedada de imagem não é oferecida; segue o
        // caminho alternativo (SVG/CSS), sem aviso.
        if agent.model.trim().is_empty() || connection.is_chatgpt_plan() {
            return None;
        }
        let client = LlmClient::from_connection(connection).ok()?;
        Some(Self::new(client, &connection.id, &agent.model, connection.wire_api, state.clone()))
    }

    pub fn new(client: LlmClient, connection_id: &str, model: &str, wire: WireApi, state: AppState) -> Self {
        Self {
            client,
            connection_id: connection_id.to_string(),
            model: model.to_string(),
            wire,
            state,
            notice: None,
            warned: AtomicBool::new(false),
            failures: AtomicUsize::new(0),
            brake: ChargedBrake::default(),
        }
    }

    /// Destino das mensagens de sistema da primeira recusa e das falhas (barramento +
    /// `dialog.jsonl`).
    pub fn with_notice(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.notice = Some(sink);
        self
    }
}

impl ImageGenerator for LlmImageGenerator {
    fn model(&self) -> &str {
        &self.model
    }

    fn available(&self) -> bool {
        self.state.image_generation_available(&self.connection_id, &self.model, self.wire)
    }

    fn generate<'a>(
        &'a self,
        prompt: &'a str,
        opts: &'a ImageOptions,
        on_notice: &'a (dyn Fn(String) + Send + Sync),
    ) -> Pin<Box<dyn Future<Output = Result<GeneratedImage, LlmError>> + Send + 'a>> {
        Box::pin(self.client.generate_image_noticed(&self.model, prompt, opts, on_notice))
    }

    fn mark_unsupported(&self) {
        let failure = ProviderFailure::BadResponse(IMAGE_UNSUPPORTED);
        log_image_failure(IMAGE_ROLE, &self.connection_id, &self.model, &failure);
        self.state.mark_image_unsupported(&self.connection_id, &self.model);
        if self.warned.swap(true, Ordering::SeqCst) {
            return;
        }
        self.system_message(IMAGE_UNAVAILABLE_NOTICE.into(), CODE_IMAGE_UNAVAILABLE, Map::new());
    }

    fn report_failure(&self, path: &str, failure: &ProviderFailure) {
        log_image_failure(IMAGE_ROLE, &self.connection_id, &self.model, failure);
        if self.failures.fetch_add(1, Ordering::SeqCst) < MAX_FAILURE_NOTICES {
            // Params neutros de idioma (spec 102): `provider`, `kind`, `provider_code` e `path`.
            let mut params = super::error::provider_failure_params(failure);
            params.insert("path".to_string(), Value::from(path));
            self.system_message(image_failure_notice(path, failure), CODE_IMAGE_FAILED, params);
        }
    }

    fn suspended(&self) -> bool {
        self.brake.suspended()
    }

    fn record_charged_failure(&self) -> bool {
        let Some(total) = self.brake.charged() else {
            return false;
        };
        log_image_suspended(IMAGE_ROLE, &self.connection_id, &self.model, total);
        // Sempre publicada (uma vez por execução: a suspensão só acontece uma vez), fora do teto.
        let params = Map::from_iter([("count".to_string(), Value::from(total))]);
        self.system_message(image_suspended_notice(total), CODE_IMAGE_SUSPENDED, params);
        true
    }

    fn record_success(&self) {
        self.brake.success();
    }
}

impl LlmImageGenerator {
    /// Publica uma mensagem de sistema da execução, quando há destino, com `code`/`params` (spec
    /// 102) e o texto de reserva em pt-BR.
    fn system_message(&self, text: String, code: &str, params: Map<String, Value>) {
        if let Some(sink) = &self.notice {
            sink.emit(RunnerEvent::SystemNotice { text, code: code.to_string(), params, task_id: None });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Connection;
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Events(Mutex<Vec<RunnerEvent>>);

    impl EventSink for Events {
        fn emit(&self, ev: RunnerEvent) {
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(ev);
        }
    }

    fn config(wire: WireApi) -> Config {
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            id: "c1".into(),
            name: "c1".into(),
            base_url: "https://api.example.com/v1".into(),
            api_key: "k".into(),
            wire_api: wire,
            kind: Default::default(),
        });
        cfg.agents.developer.connection_id = "c1".into();
        cfg.agents.developer.model = "m-img".into();
        cfg
    }

    #[tokio::test]
    async fn refusal_marks_the_pair_and_warns_once_per_execution() {
        let state = AppState::new(PathBuf::from("/tmp/nants-img-gen.toml"), Config::default(), 0);
        let events = Arc::new(Events::default());
        let generator = LlmImageGenerator::for_developer(&config(WireApi::Responses), &state)
            .expect("gerador")
            .with_notice(events.clone());
        assert_eq!(generator.model(), "m-img");
        assert!(generator.available());
        generator.mark_unsupported();
        generator.mark_unsupported();
        assert!(!generator.available(), "par marcado no registro");
        assert!(!state.image_generation_available("c1", "m-img", WireApi::Responses));
        let seen = events.0.lock().expect("lock");
        assert_eq!(seen.len(), 1, "aviso único");
        match &seen[0] {
            RunnerEvent::SystemNotice { text, code, params, .. } => {
                assert_eq!((text.as_str(), code.as_str()), (IMAGE_UNAVAILABLE_NOTICE, CODE_IMAGE_UNAVAILABLE));
                assert!(params.is_empty());
            }
            other => panic!("evento inesperado: {other:?}"),
        }
    }

    #[tokio::test]
    async fn final_failures_post_the_reason_and_path_up_to_three_per_execution() {
        let state = AppState::new(PathBuf::from("/tmp/nants-img-gen3.toml"), Config::default(), 0);
        let events = Arc::new(Events::default());
        let generator = LlmImageGenerator::for_developer(&config(WireApi::Responses), &state)
            .expect("gerador")
            .with_notice(events.clone());
        let coded = ProviderFailure::Http(400, crate::agents::llm_client::ProviderCode::parse("invalid_value"));
        generator.report_failure("web/img/a.webp", &coded);
        generator.report_failure("web/img/b.webp", &ProviderFailure::Timeout);
        generator.report_failure("web/img/c.webp", &ProviderFailure::Http(500, None));
        generator.report_failure("web/img/d.webp", &ProviderFailure::Http(500, None));
        assert!(generator.available(), "falha comum não marca o par");
        let texts: Vec<String> = events
            .0
            .lock()
            .expect("lock")
            .iter()
            .map(|ev| match ev {
                RunnerEvent::SystemNotice { text, code, params, .. } => {
                    assert_eq!(code, CODE_IMAGE_FAILED);
                    assert!(text.contains(params["path"].as_str().unwrap_or("?")), "{params:?}");
                    assert!(params.get("reason").is_none() && params.get("detail").is_none(), "spec 102: sem rótulo pt-BR: {params:?}");
                    assert!(text.contains(params["provider"].as_str().unwrap_or("?")), "{params:?}");
                    text.clone()
                }
                other => panic!("evento inesperado: {other:?}"),
            })
            .collect();
        assert_eq!(
            texts,
            [
                "Geração de imagem falhou (provider_http_400 (código: invalid_value)): web/img/a.webp. O Developer seguirá com SVG/CSS se necessário.",
                "Geração de imagem falhou (provider_timeout): web/img/b.webp. O Developer seguirá com SVG/CSS se necessário.",
                "Geração de imagem falhou (provider_http_500): web/img/c.webp. O Developer seguirá com SVG/CSS se necessário.",
            ],
            "a 4ª falha fica só no log"
        );
        // A recusa continua com o aviso único próprio, independente do teto das falhas.
        generator.mark_unsupported();
        generator.mark_unsupported();
        let seen = events.0.lock().expect("lock");
        assert_eq!(seen.len(), 4);
        assert!(matches!(&seen[3], RunnerEvent::SystemNotice { text, .. } if text == IMAGE_UNAVAILABLE_NOTICE));
    }

    /// Spec 094 T01: o aviso da suspensão sai SEMPRE, uma vez por execução, mesmo com o teto de 3
    /// mensagens de falha da 091 esgotado; o par não é marcado como indisponível.
    #[tokio::test]
    async fn suspension_notice_is_posted_once_even_with_the_failure_cap_exhausted() {
        let state = AppState::new(PathBuf::from("/tmp/nants-img-gen4.toml"), Config::default(), 0);
        let events = Arc::new(Events::default());
        let generator = LlmImageGenerator::for_developer(&config(WireApi::Responses), &state)
            .expect("gerador")
            .with_notice(events.clone());
        let unreadable = ProviderFailure::BadResponse(crate::agents::llm_client::IMAGE_UNREADABLE);
        for path in ["web/a.webp", "web/b.webp", "web/c.webp", "web/d.webp"] {
            generator.report_failure(path, &unreadable);
        }
        assert_eq!(events.0.lock().expect("lock").len(), MAX_FAILURE_NOTICES, "teto da 091 esgotado");
        assert!(!generator.suspended());
        assert!(!generator.record_charged_failure());
        generator.record_success();
        assert!(!generator.record_charged_failure(), "o sucesso zerou a contagem");
        assert!(generator.record_charged_failure(), "2 seguidas suspendem");
        assert!(generator.suspended());
        assert!(!generator.record_charged_failure(), "já suspensa");
        assert!(generator.available(), "o par não é marcado: a suspensão é da execução");
        let seen = events.0.lock().expect("lock");
        let texts: Vec<&str> = seen
            .iter()
            .map(|ev| match ev {
                RunnerEvent::SystemNotice { text, code, params, .. } => {
                    if code == CODE_IMAGE_SUSPENDED {
                        assert_eq!(params["count"], serde_json::json!(3));
                    }
                    text.as_str()
                }
                other => panic!("evento inesperado: {other:?}"),
            })
            .collect();
        assert_eq!(texts.len(), MAX_FAILURE_NOTICES + 1);
        assert!(texts[0].contains("o provedor não devolveu uma imagem legível (pode ter cobrado)"), "{}", texts[0]);
        assert_eq!(texts[MAX_FAILURE_NOTICES], image_suspended_notice(3));
        assert!(texts[MAX_FAILURE_NOTICES].contains("pode ter sido cobrada pelo provedor"));
        // Outra execução (outro gerador) começa sem suspensão.
        let next = LlmImageGenerator::for_developer(&config(WireApi::Responses), &state).expect("gerador");
        assert!(!next.suspended());
        assert_eq!(
            crate::agents::llm_client::image_suspended_line("developer", "c1", "m-img", 2),
            "nants: imagem suspensa na execução papel=developer conexão=c1 modelo=m-img falhas_cobradas=2"
        );
    }

    #[tokio::test]
    async fn chat_wire_is_unavailable_and_refuses_without_request() {
        let state = AppState::new(PathBuf::from("/tmp/nants-img-gen2.toml"), Config::default(), 0);
        let generator = LlmImageGenerator::for_developer(&config(WireApi::Chat), &state).expect("gerador");
        assert!(!generator.available());
        let err = generator
            .generate("p", &ImageOptions::default(), &|_| {})
            .await
            .expect_err("chat não gera");
        assert!(crate::agents::llm_client::is_image_unsupported(&err));
        let mut missing = config(WireApi::Responses);
        missing.agents.developer.connection_id = "nao-existe".into();
        assert!(LlmImageGenerator::for_developer(&missing, &state).is_none());
    }
}
