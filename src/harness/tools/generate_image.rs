//! `generate_image` do Desenvolvedor (spec 090 T02).
//!
//! - O gerador é injetável ([`ImageGenerator`]): em produção, o `LlmClient` da conexão e o modelo do
//!   Desenvolvedor (ferramenta hospedada `image_generation`, T01); nos testes, um falso.
//! - `path` relativo, normalizado, sem `..`, sem symlink em nenhum componente existente, dentro do
//!   `allowed_scope` e da worktree, com extensão `.webp`, `.png` ou `.jpg` coerente com o formato.
//! - O guia de arte da spec é anteposto ao prompt; a chave do cache é o SHA-256 de (guia + prompt +
//!   opções + modelo). Com o original no cache da spec, copia sem chamar o provedor.
//! - Limite de [`MAX_NEW_PER_SPEC`] gerações novas por spec (contadas no manifesto) e de
//!   [`MAX_IMAGE_BYTES`] por imagem.
//! - Geração nova registra uso com `images: 1`; o cache não registra.
//! - Recusa do provedor (`image_unsupported`): marca a capacidade (o gerador avisa uma vez por
//!   execução) e a sessão passa a responder [`IMAGE_UNAVAILABLE_MSG`] sem requisição.
//! - Nenhum texto remoto vai para o resultado: falhas viram o código fixo do provedor.
//! - Falha final (spec 091): o gerador registra o motivo (log e mensagem de sistema) e o host guarda
//!   o motivo fixo para o evento da trilha ([`ToolHost::take_image_failure`]).
//! - Freio de custo (spec 094 T01): cada HTTP 200 sem imagem aproveitável (`image_unreadable`,
//!   formato diferente do pedido, acima de 4 MiB) é uma falha cobrada do par na execução
//!   ([`ChargedBrake`], guardado no gerador). Com [`MAX_CHARGED_FAILURES`] seguidas, a geração fica
//!   suspensa no resto da execução ([`IMAGE_SUSPENDED_MSG`]); um sucesso zera a contagem.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use super::{normalize_rel, scope_key, ImageInput, SecretSet, ToolError, ToolHost, WriteScope};
use crate::agents::llm_client::{
    is_image_unreadable, is_image_unsupported, provider_failure, GeneratedImage, ProviderFailure, ImageBackground, ImageFormat, ImageOptions, ImageQuality,
    ImageSize, Usage,
};
use crate::agents::usage::{UsageEvent, UsageScope};
use crate::agents::LlmError;
use crate::sdd::images::{self, ManifestEntry, MAX_IMAGE_BYTES, MAX_NEW_PER_SPEC};

/// Resposta ao modelo quando a geração não existe neste provedor/modelo.
pub const IMAGE_UNAVAILABLE_MSG: &str =
    "geração de imagens indisponível neste provedor/modelo; use ilustração SVG/CSS equivalente";
/// Mensagem de sistema publicada (uma vez por execução) na primeira recusa.
pub const IMAGE_UNAVAILABLE_NOTICE: &str = "Este modelo não gera imagens neste provedor; usando ilustrações em SVG/CSS.";
/// Resposta ao modelo acima do limite de gerações novas da spec.
pub const IMAGE_LIMIT_MSG: &str = "limite de imagens desta spec atingido; reutilize as existentes ou use SVG/CSS";
/// Resposta quando o manifesto da spec não aceita mais uma linha (antes de gerar).
pub const IMAGE_REGISTRY_MSG: &str =
    "o registro de imagens desta spec está cheio ou indisponível; reutilize as existentes ou use SVG/CSS";
/// Resposta depois de uma geração cobrada que não pôde ser registrada (a sessão para de gerar).
pub const IMAGE_BLOCKED_MSG: &str =
    "geração de imagens suspensa nesta sessão: não foi possível registrar uma imagem gerada; use SVG/CSS";
/// Teto (caracteres) do prompt do agente.
pub const PROMPT_MAX_CHARS: usize = 2000;
/// Falhas cobradas seguidas do par que suspendem a geração no resto da execução (spec 094 T01).
pub const MAX_CHARGED_FAILURES: usize = 2;
/// Resposta ao modelo com a geração suspensa pelo freio de custo (spec 094 T01).
pub const IMAGE_SUSPENDED_MSG: &str = "geração de imagens suspensa nesta execução; use o fallback do plano";
/// Motivo fixo da recusa pela suspensão, para os FATOS (spec 092 T03).
pub const REASON_SUSPENDED: &str = "geração suspensa na execução";

/// Mensagem de sistema publicada uma vez por execução quando o freio suspende a geração (spec 094
/// decisão 1); `charged` = falhas cobradas na execução.
pub fn image_suspended_notice(charged: usize) -> String {
    format!(
        "O provedor respondeu sem uma imagem que o Nants consiga ler ({charged} tentativas); a geração pode ter sido \
         cobrada pelo provedor. As imagens restantes seguem pelo fallback do plano."
    )
}

/// Freio de custo de uma execução para um par (conexão, modelo) (spec 094 T01): conta as falhas
/// cobradas seguidas e suspende a geração ao chegar a [`MAX_CHARGED_FAILURES`].
#[derive(Debug, Default)]
pub struct ChargedBrake {
    state: Mutex<BrakeState>,
}

#[derive(Debug, Default)]
struct BrakeState {
    /// Falhas cobradas seguidas (um sucesso zera).
    streak: usize,
    /// Falhas cobradas na execução.
    total: usize,
    suspended: bool,
}

impl ChargedBrake {
    fn with_state<T>(&self, f: impl FnOnce(&mut BrakeState) -> T) -> T {
        f(&mut self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// A geração está suspensa no resto da execução.
    pub fn suspended(&self) -> bool {
        self.with_state(|s| s.suspended)
    }

    /// Registra uma falha cobrada. Devolve `Some(total na execução)` só na vez que suspende.
    pub fn charged(&self) -> Option<usize> {
        self.with_state(|s| {
            s.streak += 1;
            s.total += 1;
            if s.suspended || s.streak < MAX_CHARGED_FAILURES {
                return None;
            }
            s.suspended = true;
            Some(s.total)
        })
    }

    /// Imagem aproveitada: zera a sequência (a suspensão, se houver, continua).
    pub fn success(&self) {
        self.with_state(|s| s.streak = 0);
    }
}

/// Aviso da espera do ritmo antes de gerar (a atividade o publica com `code`, spec 102).
pub const IMAGE_PACE_NOTICE: &str = "aguardando o ritmo";

/// Avisos fixos de espera/nova tentativa durante a geração.
pub type Notice<'a> = &'a (dyn Fn(String) + Send + Sync);

/// Gerador de imagens injetável (spec 090 T02).
pub trait ImageGenerator: Send + Sync {
    /// Modelo que gera (entra na chave do cache e no registro de uso).
    fn model(&self) -> &str;

    /// A geração está disponível agora para o par (conexão, modelo).
    fn available(&self) -> bool;

    /// Gera a imagem de `prompt` (já com o guia anteposto).
    fn generate<'a>(
        &'a self,
        prompt: &'a str,
        opts: &'a ImageOptions,
        on_notice: Notice<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<GeneratedImage, LlmError>> + Send + 'a>>;

    /// O provedor recusou a geração: marca a capacidade como indisponível e avisa o usuário (o
    /// gerador garante o aviso único por execução e registra a linha de log, spec 091).
    fn mark_unsupported(&self);

    /// Falha final da geração de `path` (spec 091), fora a recusa de capacidade: registra o motivo
    /// fixo (log e mensagem de sistema; o gerador limita as mensagens por execução).
    fn report_failure(&self, path: &str, failure: &ProviderFailure);

    /// Geração suspensa no resto da execução pelo freio de custo (spec 094 T01).
    fn suspended(&self) -> bool;

    /// Resposta HTTP 200 sem imagem aproveitável (possivelmente cobrada). Devolve `true` quando esta
    /// falha suspende a geração; o gerador publica então o aviso único da execução, fora do teto
    /// das falhas da 091.
    fn record_charged_failure(&self) -> bool;

    /// Imagem aproveitada: zera a contagem do freio.
    fn record_success(&self);
}

/// Ligação de `generate_image` a um host: gerador, pasta da spec no repositório principal (cache e
/// manifesto), tarefa, guia de arte, uso e segredos para redigir o manifesto.
pub struct ImageGenLink {
    pub generator: Arc<dyn ImageGenerator>,
    pub spec_dir: PathBuf,
    pub task_id: String,
    /// Texto da seção "## Guia de arte" (≤ 1500 caracteres); vazio sem ela.
    pub guide: String,
    pub usage: Option<UsageScope>,
    pub secrets: Option<Arc<SecretSet>>,
}

/// Pedido validado.
struct ImageRequest {
    /// Chave relativa normalizada (`/`).
    key: String,
    rel: PathBuf,
    prompt: String,
    opts: ImageOptions,
}

/// Valor de um enum fechado: ausente/nulo = padrão; texto fora da lista ou outro tipo = erro.
fn enum_arg<T: Default>(args: &Value, key: &str, allowed: &str, parse: fn(&str) -> Option<T>) -> Result<T, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(Value::String(s)) => {
            parse(s.trim()).ok_or_else(|| ToolError::InvalidArgs(format!("`{key}` deve ser um de: {allowed}")))
        }
        Some(_) => Err(ToolError::InvalidArgs(format!("`{key}` deve ser um de: {allowed}"))),
    }
}

/// Formato da API para o texto de `images::format_of_path`.
fn api_format(name: &str) -> ImageFormat {
    ImageFormat::parse(name).unwrap_or_default()
}

/// `N KB` arredondado para cima.
fn kb(bytes: usize) -> usize {
    bytes.div_ceil(1024)
}

impl ToolHost {
    /// `generate_image {path, prompt, size?, background?, quality?}`.
    ///
    /// Antes do provedor: argumentos, destino (pais existentes são diretórios reais) e folga no
    /// manifesto. Depois de uma geração (cobrada), o uso e a linha do manifesto são registrados em
    /// todos os desfechos — gravada, recusada por tamanho/formato ou com falha de gravação
    /// (`stored: false`) —, então toda geração conta no limite. Se a linha não puder ser registrada,
    /// a sessão passa a recusar `generate_image` (fail-closed).
    pub(super) async fn generate_image(&self, args: &Value, on_notice: Notice<'_>) -> Result<String, ToolError> {
        let Some(link) = &self.image_gen else {
            return Err(ToolError::Denied("generate_image indisponível neste contexto: use ilustração SVG/CSS".into()));
        };
        if self.image_unavailable.load(Ordering::SeqCst) || !link.generator.available() {
            return Ok(IMAGE_UNAVAILABLE_MSG.into());
        }
        if self.image_blocked.load(Ordering::SeqCst) {
            self.note_image_refusal(images::REASON_BLOCKED);
            return Err(ToolError::Denied(IMAGE_BLOCKED_MSG.into()));
        }
        let req = self.image_request(args)?;
        let target = self.image_target(&req.rel)?;
        if !images::manifest_has_room(&link.spec_dir) {
            self.note_image_refusal(images::REASON_REGISTRY_FULL);
            return Err(ToolError::Denied(IMAGE_REGISTRY_MSG.into()));
        }
        let format = req.opts.format.as_str();
        let options = [req.opts.size.as_str(), req.opts.background.as_str(), req.opts.quality.as_str(), format];
        let hash = images::cache_key(&link.guide, &req.prompt, options, link.generator.model());
        if let Some(bytes) = images::read_cached(&link.spec_dir, &hash, format) {
            // Reaproveitada: sem custo; o registro é melhor esforço (não conta no limite).
            write_target(&target, &bytes)?;
            let _ = images::append_manifest(&link.spec_dir, &self.manifest_entry(link, &req, &hash, &bytes, true, true));
            self.push_image(req.key.clone(), ImageInput::new(images::media_type(format), bytes.clone()));
            let dims = images::dims_text(images::dimensions(&bytes));
            return Ok(format!("imagem gravada em {} ({dims}, {} KB) (reaproveitada)", req.key, kb(bytes.len())));
        }
        if images::new_generations(&images::read_manifest(&link.spec_dir)) >= MAX_NEW_PER_SPEC {
            self.note_image_refusal(images::REASON_LIMIT);
            return Ok(IMAGE_LIMIT_MSG.into());
        }
        // Freio de custo (spec 094 T01): suspensa na execução, sem pedido ao provedor.
        if link.generator.suspended() {
            self.note_image_refusal(REASON_SUSPENDED);
            return Ok(IMAGE_SUSPENDED_MSG.into());
        }
        let prompt = images::compose_prompt(&link.guide, &req.prompt);
        let image = match link.generator.generate(&prompt, &req.opts, on_notice).await {
            Ok(image) => image,
            Err(e) if is_image_unsupported(&e) => {
                self.image_unavailable.store(true, Ordering::SeqCst);
                link.generator.mark_unsupported();
                return Ok(IMAGE_UNAVAILABLE_MSG.into());
            }
            // Só o código fixo (com o código saneado, spec 091): nada do corpo ou do texto do
            // provedor. O motivo vai também para o log, a conversa e a trilha.
            Err(e) => {
                let failure = provider_failure(&e);
                link.generator.report_failure(&req.key, &failure);
                let reason = failure.describe();
                *self.image_failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.clone());
                *self.last_image_failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(reason.clone());
                // HTTP 200 sem imagem legível: possivelmente cobrada (spec 094 T01).
                if is_image_unreadable(&e) && link.generator.record_charged_failure() {
                    return Err(ToolError::Denied(format!("a geração da imagem falhou ({reason}); {IMAGE_SUSPENDED_MSG}")));
                }
                return Err(ToolError::Denied(format!(
                    "a geração da imagem falhou ({reason}); tente de novo mais tarde ou use SVG/CSS"
                )));
            }
        };
        // A geração foi cobrada: uso e contagem valem para qualquer desfecho abaixo.
        record_usage(link, image.usage, req.opts.quality.as_str());
        let bytes = image.bytes;
        // Recebida mas não aproveitável (tamanho ou formato): falha cobrada do freio (spec 094 T01).
        let mut charged = true;
        let outcome = if bytes.len() as u64 > MAX_IMAGE_BYTES {
            Err(ToolError::Denied(format!(
                "a imagem gerada tem {} KB, acima do limite de 4 MiB, e não foi gravada; peça quality ou size menor",
                kb(bytes.len())
            )))
        } else if image.format != req.opts.format || crate::sdd::references::sniff(&bytes) != Some(image.format.media_type()) {
            self.note_image_refusal(images::REASON_FORMAT);
            Err(ToolError::Denied(format!(
                "o provedor devolveu um formato diferente do pedido ({}) e a imagem não foi gravada; use a extensão .{}",
                format,
                images::ext_of_format(image.format.as_str())
            )))
        } else {
            charged = false;
            // Cache melhor esforço (sem ele, só não haverá reaproveitamento); o destino é obrigatório.
            let _ = images::store_cached(&link.spec_dir, &hash, format, &bytes);
            write_target(&target, &bytes)
        };
        // Falha de gravação local não é do provedor: não conta nem zera.
        let suspended_now = match (&outcome, charged) {
            (Ok(()), _) => {
                link.generator.record_success();
                false
            }
            (Err(_), true) => link.generator.record_charged_failure(),
            (Err(_), false) => false,
        };
        let outcome = match outcome {
            Err(ToolError::Denied(msg)) if suspended_now => Err(ToolError::Denied(format!("{msg}; {IMAGE_SUSPENDED_MSG}"))),
            other => other,
        };
        let entry = self.manifest_entry(link, &req, &hash, &bytes, false, outcome.is_ok());
        if images::append_manifest(&link.spec_dir, &entry).is_err() {
            self.image_blocked.store(true, Ordering::SeqCst);
            self.note_image_refusal(images::REASON_BLOCKED);
            return Err(ToolError::Denied(IMAGE_BLOCKED_MSG.into()));
        }
        outcome?;
        self.push_image(req.key.clone(), ImageInput::new(images::media_type(format), bytes.clone()));
        Ok(format!("imagem gravada em {} ({}, {} KB)", req.key, entry.dims(), kb(bytes.len())))
    }

    /// Valida os argumentos: caminho, extensão, prompt e enums fechados.
    fn image_request(&self, args: &Value) -> Result<ImageRequest, ToolError> {
        let raw = super::arg(args, "path")?.trim();
        let rel = normalize_rel(raw)?;
        let key = scope_key(&rel);
        if key.is_empty() {
            return Err(ToolError::InvalidArgs("`path` vazio".into()));
        }
        let format = images::format_of_path(&key)
            .ok_or_else(|| ToolError::InvalidArgs("`path` deve terminar em .webp, .png ou .jpg".into()))?;
        let prompt = super::arg(args, "prompt")?.trim().to_string();
        let chars = prompt.chars().count();
        if chars == 0 || chars > PROMPT_MAX_CHARS {
            return Err(ToolError::InvalidArgs(format!("`prompt` deve ter de 1 a {PROMPT_MAX_CHARS} caracteres")));
        }
        let opts = ImageOptions {
            size: enum_arg(args, "size", "1024x1024, 1536x1024, 1024x1536, auto", ImageSize::parse)?,
            background: enum_arg(args, "background", "transparent, opaque, auto", ImageBackground::parse)?,
            quality: enum_arg(args, "quality", "low, medium, high", ImageQuality::parse)?,
            format: api_format(format),
        };
        if opts.background == ImageBackground::Transparent && opts.format == ImageFormat::Jpeg {
            return Err(ToolError::InvalidArgs("JPEG não tem fundo transparente: use .png ou .webp".into()));
        }
        Ok(ImageRequest { key, rel, prompt, opts })
    }

    /// Destino real na worktree: nenhum componente existente é symlink, os pais existentes são
    /// diretórios (o que falta será criado), o próprio destino, se existir, é arquivo regular, e o
    /// caminho (lexical e real) casa o `allowed_scope`.
    fn image_target(&self, rel: &std::path::Path) -> Result<PathBuf, ToolError> {
        let parts: Vec<_> = rel.components().collect();
        let mut current = self.root.clone();
        for (i, part) in parts.iter().enumerate() {
            current.push(part);
            let last = i + 1 == parts.len();
            match std::fs::symlink_metadata(&current) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(ToolError::InvalidArgs(format!("caminho com symlink: {}", scope_key(rel))));
                }
                Ok(m) if !last && !m.is_dir() => {
                    return Err(ToolError::InvalidArgs(format!(
                        "um componente do caminho não é diretório: {}",
                        scope_key(&rel.iter().take(i + 1).collect::<PathBuf>())
                    )));
                }
                Ok(m) if last && !m.is_file() => {
                    return Err(ToolError::InvalidArgs("destino não é arquivo regular".into()));
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        self.check_write_target(rel, WriteScope::Allowed)
    }

    /// Linha do manifesto da imagem (prompt redigido e truncado; dimensões do cabeçalho).
    fn manifest_entry(
        &self,
        link: &ImageGenLink,
        req: &ImageRequest,
        hash: &str,
        bytes: &[u8],
        reused: bool,
        stored: bool,
    ) -> ManifestEntry {
        let dims = images::dimensions(bytes);
        let prompt = match &link.secrets {
            Some(s) => s.scrub_exact(&req.prompt),
            None => req.prompt.clone(),
        };
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        ManifestEntry {
            path: req.key.clone(),
            hash: hash.to_string(),
            prompt: images::clip(&prompt, images::MANIFEST_PROMPT_MAX_CHARS),
            width: dims.map(|d| d.0),
            height: dims.map(|d| d.1),
            bytes: bytes.len() as u64,
            task: link.task_id.clone(),
            at: crate::server::projects::format_utc(secs),
            reused,
            stored,
        }
    }
}

/// Grava a imagem no destino da worktree (pais criados; escrita atômica, sem seguir symlink).
fn write_target(target: &std::path::Path, bytes: &[u8]) -> Result<(), ToolError> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    images::write_atomic(target, bytes)?;
    Ok(())
}

/// Uso da geração nova no `usage.jsonl` (papel do escopo, `images: 1` e a qualidade pedida).
fn record_usage(link: &ImageGenLink, usage: Option<Usage>, quality: &str) {
    if let Some(scope) = &link.usage {
        scope.sink.record(UsageEvent {
            agent: scope.agent.clone(),
            model: link.generator.model().to_string(),
            spec: scope.spec.clone(),
            task: scope.task.clone(),
            usage: usage.unwrap_or(Usage::new(0, 0)),
            web_search_calls: 0,
            images: 1,
            image_quality: Some(quality.to_string()),
            estimated: false,
            cut: None,
            billing: Default::default(),
        });
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// Gerador falso: guarda os prompts recebidos e responde com `reply`.
    pub struct FakeGenerator {
        pub prompts: Mutex<Vec<String>>,
        pub available: AtomicBool,
        pub marks: AtomicUsize,
        /// Falhas finais relatadas: (caminho, motivo `describe`).
        pub failures: Mutex<Vec<(String, String)>>,
        /// Freio de custo da "execução" (spec 094 T01).
        pub brake: ChargedBrake,
        /// Avisos de suspensão publicados (texto).
        pub suspension_notices: Mutex<Vec<String>>,
        #[allow(clippy::type_complexity)]
        pub reply: Box<dyn Fn(&ImageOptions) -> Result<GeneratedImage, LlmError> + Send + Sync>,
    }

    impl FakeGenerator {
        pub fn new(reply: impl Fn(&ImageOptions) -> Result<GeneratedImage, LlmError> + Send + Sync + 'static) -> Self {
            Self {
                prompts: Mutex::new(Vec::new()),
                available: AtomicBool::new(true),
                marks: AtomicUsize::new(0),
                failures: Mutex::new(Vec::new()),
                brake: ChargedBrake::default(),
                suspension_notices: Mutex::new(Vec::new()),
                reply: Box::new(reply),
            }
        }

        /// Responde sempre com um PNG de `w`×`h` (o formato pedido é ignorado: use `.png`).
        pub fn png(w: u32, h: u32) -> Self {
            Self::new(move |_| Ok(image(crate::sdd::images::testing::png(w, h), ImageFormat::Png)))
        }

        pub fn calls(&self) -> usize {
            self.prompts.lock().map(|p| p.len()).unwrap_or(0)
        }
    }

    pub fn image(bytes: Vec<u8>, format: ImageFormat) -> GeneratedImage {
        GeneratedImage { bytes, format, revised_prompt: Some("texto remoto".into()), usage: Some(Usage::new(10, 1500)) }
    }

    impl ImageGenerator for FakeGenerator {
        fn model(&self) -> &str {
            "m-img"
        }

        fn available(&self) -> bool {
            self.available.load(Ordering::SeqCst)
        }

        fn generate<'a>(
            &'a self,
            prompt: &'a str,
            opts: &'a ImageOptions,
            on_notice: Notice<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<GeneratedImage, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                self.prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(prompt.to_string());
                on_notice(IMAGE_PACE_NOTICE.into());
                (self.reply)(opts)
            })
        }

        fn mark_unsupported(&self) {
            self.marks.fetch_add(1, Ordering::SeqCst);
            self.available.store(false, Ordering::SeqCst);
        }

        fn report_failure(&self, path: &str, failure: &ProviderFailure) {
            self.failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((path.to_string(), failure.describe()));
        }

        fn suspended(&self) -> bool {
            self.brake.suspended()
        }

        fn record_charged_failure(&self) -> bool {
            let Some(total) = self.brake.charged() else {
                return false;
            };
            self.suspension_notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(image_suspended_notice(total));
            true
        }

        fn record_success(&self) {
            self.brake.success();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{image, FakeGenerator};
    use super::*;
    use crate::agents::llm_client::IMAGE_UNSUPPORTED;
    use crate::agents::usage::testing::MemSink;
    use crate::harness::allowlist::Policy;
    use crate::harness::tools::{ApprovalDecision, ApprovalRequest, Approver, Role, ToolName};
    use crate::sdd::images::testing::{png, webp};
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use std::sync::Mutex;

    struct Deny;

    impl Approver for Deny {
        fn approve<'a>(&'a self, _req: &'a ApprovalRequest) -> Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'a>> {
            Box::pin(async { ApprovalDecision::Deny })
        }
    }

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        spec_dir: PathBuf,
        sink: Arc<MemSink>,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!("nants-genimg-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let root = base.join("wt");
            let spec_dir = base.join("repo/.sdd/specs/090");
            fs::create_dir_all(root.join("web")).expect("wt");
            fs::create_dir_all(&spec_dir).expect("spec");
            Self { base, root, spec_dir, sink: Arc::new(MemSink::default()) }
        }

        fn host(&self, generator: &Arc<FakeGenerator>, task: &str, guide: &str) -> ToolHost {
            let secrets = Arc::new(SecretSet::new(vec!["sk-SEGREDO-123456".to_string()]));
            ToolHost::new(
                Role::Developer,
                self.root.clone(),
                vec!["web/**".into()],
                Policy::new(Vec::new(), Vec::new()),
                Arc::new(Deny),
            )
            .with_image_generation(ImageGenLink {
                generator: Arc::clone(generator) as Arc<dyn ImageGenerator>,
                spec_dir: self.spec_dir.clone(),
                task_id: task.into(),
                guide: guide.into(),
                usage: Some(UsageScope {
                    sink: self.sink.clone(),
                    agent: "developer".into(),
                    spec: Some("090".into()),
                    task: Some(task.into()),
                }),
                secrets: Some(secrets),
            })
        }

        fn usage(&self) -> Vec<UsageEvent> {
            self.sink.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn gen(path: &str, prompt: &str) -> Value {
        json!({"path": path, "prompt": prompt})
    }

    async fn call(host: &ToolHost, args: Value) -> Result<String, ToolError> {
        host.call(ToolName::GenerateImage, args).await
    }

    #[tokio::test]
    async fn writes_the_image_records_manifest_usage_and_queues_it() {
        let fx = Fixture::new("ok");
        let generator = Arc::new(FakeGenerator::png(1536, 1024));
        let host = fx.host(&generator, "T01", "");
        let notices = Mutex::new(Vec::new());
        let on_notice = |n: String| notices.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(n);
        let args = json!({"path": "./web/img/gato.png", "prompt": "um gato com sk-SEGREDO-123456", "size": "1536x1024"});
        let out = host.call_noticed(ToolName::GenerateImage, args, &on_notice).await.expect("gera");
        assert_eq!(out, "imagem gravada em web/img/gato.png (1536×1024, 1 KB)");
        assert_eq!(fs::read(fx.root.join("web/img/gato.png")).expect("arquivo"), png(1536, 1024));
        assert_eq!(notices.lock().expect("lock").as_slice(), ["aguardando o ritmo"], "avisos repassados");
        // Prompt puro, sem guia.
        assert_eq!(generator.prompts.lock().expect("lock").as_slice(), ["um gato com sk-SEGREDO-123456"]);
        // Manifesto: prompt redigido; o texto remoto (`revised_prompt`) não é guardado.
        let manifest = images::read_manifest(&fx.spec_dir);
        assert_eq!(manifest.len(), 1);
        let e = &manifest[0];
        assert_eq!((e.path.as_str(), e.task.as_str(), e.width, e.height, e.reused), ("web/img/gato.png", "T01", Some(1536), Some(1024), false));
        assert_eq!(e.prompt, "um gato com ***");
        let raw = fs::read_to_string(fx.spec_dir.join(images::MANIFEST_FILE)).expect("manifesto");
        assert!(!raw.contains("SEGREDO") && !raw.contains("texto remoto"), "{raw}");
        assert!(fx.spec_dir.join(format!("images/{}.png", e.hash)).is_file(), "original no cache");
        // Uso com `images: 1`.
        let usage = fx.usage();
        assert_eq!(usage.len(), 1);
        assert_eq!((usage[0].images, usage[0].model.as_str(), usage[0].task.as_deref()), (1, "m-img", Some("T01")));
        assert_eq!(usage[0].usage, Usage::new(10, 1500));
        // Spec 092 T03: a qualidade pedida (padrão medium) entra no uso, para o custo da imagem.
        assert_eq!(usage[0].image_quality.as_deref(), Some("medium"));
        // A imagem vai para o canal de imagens.
        let queued = host.take_images();
        assert_eq!(queued.len(), 1);
        assert_eq!((queued[0].name.as_str(), queued[0].image.media_type.as_str()), ("web/img/gato.png", "image/png"));
    }

    #[tokio::test]
    async fn guide_is_prepended_and_changes_the_cache_key() {
        let fx = Fixture::new("guide");
        let generator = Arc::new(FakeGenerator::png(8, 8));
        fx.host(&generator, "T01", "Aquarela, traço fino").call(ToolName::GenerateImage, gen("web/a.png", "um gato")).await.expect("1");
        let prompts = generator.prompts.lock().expect("lock").clone();
        assert!(prompts[0].starts_with("Guia de arte") && prompts[0].contains("Aquarela, traço fino"), "{}", prompts[0]);
        assert!(prompts[0].find("Aquarela") < prompts[0].find("um gato"));
        // Mesmo prompt sem guia: outra chave, gera de novo.
        call(&fx.host(&generator, "T01", ""), gen("web/b.png", "um gato")).await.expect("2");
        assert_eq!(generator.calls(), 2);
        assert_eq!(generator.prompts.lock().expect("lock")[1], "um gato");
    }

    #[tokio::test]
    async fn same_combination_is_copied_from_cache_without_the_provider() {
        let fx = Fixture::new("cache");
        let generator = Arc::new(FakeGenerator::png(64, 32));
        let host = fx.host(&generator, "T01", "g");
        call(&host, gen("web/a.png", "um gato")).await.expect("1");
        let other_task = fx.host(&generator, "T02", "g");
        let out = call(&other_task, gen("web/copia.png", "um gato")).await.expect("cache");
        assert_eq!(out, "imagem gravada em web/copia.png (64×32, 1 KB) (reaproveitada)");
        assert_eq!(generator.calls(), 1, "sem nova chamada ao provedor");
        assert_eq!(fs::read(fx.root.join("web/copia.png")).expect("cópia"), png(64, 32));
        assert_eq!(fx.usage().len(), 1, "o cache não registra uso");
        let manifest = images::read_manifest(&fx.spec_dir);
        assert_eq!(manifest.len(), 2);
        assert!(manifest[1].reused && manifest[1].task == "T02" && manifest[1].hash == manifest[0].hash);
        assert_eq!(other_task.take_images().len(), 1, "reaproveitada também vai para o canal");
        // Opção diferente: outra chave.
        call(&host, json!({"path": "web/a.png", "prompt": "um gato", "quality": "high"})).await.expect("3");
        assert_eq!(generator.calls(), 2);
    }

    #[tokio::test]
    async fn path_validation_scope_dotdot_symlink_and_extension() {
        let fx = Fixture::new("paths");
        let generator = Arc::new(FakeGenerator::png(8, 8));
        let host = fx.host(&generator, "T01", "");
        let outside = fx.base.join("fora");
        fs::create_dir_all(&outside).expect("fora");
        symlink(&outside, fx.root.join("web/link")).expect("link dir");
        fs::write(outside.join("x.png"), b"intacto").expect("x");
        symlink(outside.join("x.png"), fx.root.join("web/alvo.png")).expect("link arquivo");
        let cases = [
            ("src/a.png", "fora do allowed_scope"),
            ("../wt/web/a.png", "fora da raiz"),
            ("web/../../a.png", "fora da raiz"),
            ("/tmp/a.png", "fora da raiz"),
            ("web/link/a.png", "symlink"),
            ("web/alvo.png", "symlink"),
            ("web/a.gif", ".webp, .png ou .jpg"),
            ("web/a", ".webp, .png ou .jpg"),
            ("", "vazio"),
        ];
        for (path, why) in cases {
            let err = call(&host, gen(path, "um gato")).await.expect_err(path);
            assert!(err.to_string().contains(why), "{path}: {err}");
        }
        assert_eq!(fs::read(outside.join("x.png")).expect("x"), b"intacto");
        assert!(!outside.join("a.png").exists());
        // Prompt e enums.
        for (args, why) in [
            (gen("web/a.png", "  "), "1 a 2000"),
            (gen("web/a.png", &"x".repeat(2001)), "1 a 2000"),
            (json!({"path": "web/a.png", "prompt": "p", "size": "512x512"}), "`size`"),
            (json!({"path": "web/a.png", "prompt": "p", "background": "verde"}), "`background`"),
            (json!({"path": "web/a.png", "prompt": "p", "quality": 3}), "`quality`"),
            (json!({"path": "web/a.jpg", "prompt": "p", "background": "transparent"}), "transparente"),
        ] {
            let err = call(&host, args).await.expect_err(why);
            assert!(err.to_string().contains(why), "{why}: {err}");
        }
        assert_eq!(generator.calls(), 0, "nada inválido chega ao provedor");
        // 2000 caracteres e `.jpg` opaco passam.
        call(&host, gen("web/ok.png", &"é".repeat(2000))).await.expect("2000");
    }

    #[tokio::test]
    async fn limit_of_new_generations_per_spec() {
        let fx = Fixture::new("limit");
        let generator = Arc::new(FakeGenerator::png(8, 8));
        let host = fx.host(&generator, "T01", "");
        for i in 0..MAX_NEW_PER_SPEC {
            call(&host, gen(&format!("web/{i}.png"), &format!("imagem {i}"))).await.expect("dentro do limite");
        }
        let over = call(&host, gen("web/x.png", "mais uma")).await.expect("limite");
        assert_eq!(over, IMAGE_LIMIT_MSG);
        // Spec 092 T03: a recusa pelo limite fica registrada para os FATOS (nunca "disponível").
        assert_eq!(host.last_image_failure().as_deref(), Some(images::REASON_LIMIT));
        assert_eq!(generator.calls(), MAX_NEW_PER_SPEC);
        // O cache continua valendo acima do limite.
        let reused = call(&host, gen("web/y.png", "imagem 0")).await.expect("cache");
        assert!(reused.ends_with("(reaproveitada)"), "{reused}");
        assert!(!fx.root.join("web/x.png").exists());
    }

    #[tokio::test]
    async fn image_over_4_mib_is_refused_without_writing_but_counts() {
        let fx = Fixture::new("big");
        let generator = Arc::new(FakeGenerator::new(|_| {
            let mut bytes = png(4096, 4096);
            bytes.resize((MAX_IMAGE_BYTES + 1) as usize, 0);
            Ok(image(bytes, ImageFormat::Png))
        }));
        let host = fx.host(&generator, "T01", "");
        let err = call(&host, gen("web/grande.png", "um gato")).await.expect_err("grande");
        assert!(err.to_string().contains("4 MiB"), "{err}");
        assert!(!fx.root.join("web/grande.png").exists());
        assert!(host.take_images().is_empty());
        assert_eq!(fx.usage().len(), 1, "a geração cobrada entra no uso");
        // Cobrada: conta no limite, sem aparecer como imagem da spec.
        let manifest = images::read_manifest(&fx.spec_dir);
        assert_eq!(manifest.len(), 1);
        assert!(!manifest[0].stored && !manifest[0].reused);
        assert_eq!(images::new_generations(&manifest), 1);
        assert!(images::SpecImages::load(&fx.spec_dir).view_names().is_empty());
    }

    #[tokio::test]
    async fn webp_dimensions_and_mismatched_format() {
        let fx = Fixture::new("webp");
        let generator = Arc::new(FakeGenerator::new(|opts| match opts.format {
            ImageFormat::Webp => Ok(image(webp(1024, 1536), ImageFormat::Webp)),
            _ => Ok(image(png(8, 8), ImageFormat::Png)),
        }));
        let host = fx.host(&generator, "T01", "");
        let out = call(&host, gen("web/a.webp", "um gato")).await.expect("webp");
        assert_eq!(out, "imagem gravada em web/a.webp (1024×1536, 1 KB)");
        // `.jpg` pedido, PNG devolvido: recusa clara, nada gravado.
        let err = call(&host, gen("web/b.jpg", "um gato")).await.expect_err("formato");
        assert!(err.to_string().contains("use a extensão .png"), "{err}");
        assert!(!fx.root.join("web/b.jpg").exists());
        // A recusada também conta no limite.
        let manifest = images::read_manifest(&fx.spec_dir);
        assert_eq!(images::new_generations(&manifest), 2);
        assert!(manifest[0].stored && !manifest[1].stored);
    }

    #[tokio::test]
    async fn parent_that_is_a_file_is_refused_before_the_provider() {
        let fx = Fixture::new("parent-file");
        fs::write(fx.root.join("web/README.txt"), "texto").expect("arquivo");
        let generator = Arc::new(FakeGenerator::png(8, 8));
        let host = fx.host(&generator, "T01", "");
        for (i, path) in ["web/README.txt/a.png", "web/README.txt/sub/b.png"].into_iter().enumerate() {
            let err = call(&host, gen(path, &format!("prompt {i}"))).await.expect_err(path);
            assert!(err.to_string().contains("não é diretório: web/README.txt"), "{err}");
        }
        assert_eq!(generator.calls(), 0);
        assert!(images::read_manifest(&fx.spec_dir).is_empty() && fx.usage().is_empty());
        fs::create_dir_all(fx.root.join("web/pasta.png")).expect("pasta");
        let err = call(&host, gen("web/pasta.png", "p")).await.expect_err("diretório");
        assert!(err.to_string().contains("não é arquivo regular"), "{err}");
        assert_eq!(generator.calls(), 0);
    }

    #[tokio::test]
    async fn full_or_missing_manifest_refuses_before_generating() {
        let fx = Fixture::new("full");
        let generator = Arc::new(FakeGenerator::png(8, 8));
        let host = fx.host(&generator, "T01", "");
        let filler = vec![b'\n'; (images::MAX_MANIFEST_BYTES - 100) as usize];
        fs::write(fx.spec_dir.join(images::MANIFEST_FILE), filler).expect("cheio");
        let err = call(&host, gen("web/a.png", "p")).await.expect_err("cheio");
        assert_eq!(err.to_string(), IMAGE_REGISTRY_MSG);
        assert_eq!(host.last_image_failure().as_deref(), Some(images::REASON_REGISTRY_FULL));
        assert_eq!(generator.calls(), 0);
        assert!(!fx.root.join("web/a.png").exists());
        // Pasta da spec ausente: também recusa antes de gerar.
        fs::remove_dir_all(&fx.spec_dir).expect("rm spec");
        assert_eq!(call(&host, gen("web/a.png", "p")).await.expect_err("sem spec").to_string(), IMAGE_REGISTRY_MSG);
        assert_eq!(generator.calls(), 0);
    }

    #[tokio::test]
    async fn unregistered_generation_blocks_the_session() {
        let fx = Fixture::new("fail-closed");
        // O manifesto some (vira pasta) durante a geração: a linha não pode ser gravada.
        let manifest = fx.spec_dir.join(images::MANIFEST_FILE);
        let generator = Arc::new(FakeGenerator::new(move |_| {
            let _ = fs::create_dir(&manifest);
            Ok(image(png(8, 8), ImageFormat::Png))
        }));
        let host = fx.host(&generator, "T01", "");
        let err = call(&host, gen("web/a.png", "p")).await.expect_err("sem registro");
        assert_eq!(err.to_string(), IMAGE_BLOCKED_MSG);
        assert_eq!(fx.usage().len(), 1, "a cobrança entra no uso");
        // A sessão não gera mais, nem com outro prompt.
        let again = call(&host, gen("web/b.png", "q")).await.expect_err("bloqueada");
        assert_eq!(again.to_string(), IMAGE_BLOCKED_MSG);
        assert_eq!(generator.calls(), 1);
    }

    #[tokio::test]
    async fn truncated_cache_is_regenerated_and_no_temporary_is_left() {
        let fx = Fixture::new("trunc");
        let generator = Arc::new(FakeGenerator::png(32, 16));
        let host = fx.host(&generator, "T01", "");
        call(&host, gen("web/img/a.png", "um gato")).await.expect("1");
        let hash = images::read_manifest(&fx.spec_dir)[0].hash.clone();
        let cached = fx.spec_dir.join(format!("images/{hash}.png"));
        let full = fs::read(&cached).expect("cache");
        fs::write(&cached, &full[..full.len() - 5]).expect("trunca");
        // O truncado não é servido como "(reaproveitada)": gera de novo e regrava o cache inteiro.
        let out = call(&host, gen("web/img/b.png", "um gato")).await.expect("2");
        assert!(!out.contains("reaproveitada"), "{out}");
        assert_eq!(generator.calls(), 2);
        assert_eq!(fs::read(&cached).expect("cache"), full);
        assert_eq!(fs::read(fx.root.join("web/img/b.png")).expect("b"), full);
        for dir in [fx.root.join("web/img"), fx.spec_dir.join("images")] {
            let names: Vec<String> =
                fs::read_dir(&dir).expect("dir").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            assert!(names.iter().all(|n| !n.ends_with(".tmp")), "{names:?}");
        }
        // O Revisor também não recebe o original truncado.
        fs::write(&cached, &full[..full.len() - 5]).expect("trunca");
        let imgs = images::SpecImages::load(&fx.spec_dir);
        let entry = imgs.resolve("a.png").expect("entrada");
        assert!(imgs.read(entry).is_err());
    }

    #[tokio::test]
    async fn view_prefers_a_capture_with_the_same_name_and_lists_every_source() {
        use crate::harness::tools::ScreenshotSource;
        let fx = Fixture::new("view");
        let generator = Arc::new(FakeGenerator::png(16, 16));
        call(&fx.host(&generator, "T01", ""), gen("web/img/gato.png", "um gato")).await.expect("gera");
        fs::create_dir_all(fx.root.join("shots")).expect("shots");
        fs::write(fx.root.join("shots/gato.png"), png(2, 2)).expect("captura");
        let refs_dir = fx.base.join("refs-spec");
        fs::create_dir_all(refs_dir.join("references")).expect("refs");
        let reference = format!("{}.png", "c".repeat(32));
        fs::write(refs_dir.join("references").join(&reference), png(1, 1)).expect("ref");
        let reviewer = || {
            ToolHost::new(Role::Reviewer, fx.root.clone(), Vec::new(), Policy::new(Vec::new(), Vec::new()), Arc::new(Deny))
                .with_generated_images(images::SpecImages::load(&fx.spec_dir))
        };
        let view = |name: &str| json!({"name": name});
        // Mesmo nome curto nas capturas e nas imagens geradas: vale a captura.
        let h = reviewer().with_screenshots(ScreenshotSource::Fixed(vec!["shots/gato.png".into()]));
        let out = h.call(ToolName::ViewScreenshot, view("gato.png")).await.expect("captura");
        assert_eq!(out, "captura shots/gato.png anexada na mensagem seguinte");
        let out = h.call(ToolName::ViewScreenshot, view("web/img/gato.png")).await.expect("gerada");
        assert_eq!(out, "imagem gerada web/img/gato.png anexada na mensagem seguinte");
        // Nome inválido: o erro lista capturas, imagens geradas e referências.
        let h = h.with_references(crate::sdd::references::References::load(&refs_dir));
        let err = h.call(ToolName::ViewScreenshot, view("nada.png")).await.expect_err("inválido");
        let text = err.to_string();
        assert!(text.contains("shots/gato.png") && text.contains("web/img/gato.png") && text.contains(&reference), "{text}");
        // Sem capturas: imagens geradas e referências.
        let h = reviewer().with_references(crate::sdd::references::References::load(&refs_dir));
        let text = h.call(ToolName::ViewScreenshot, view("nada.png")).await.expect_err("inválido").to_string();
        assert!(text.contains("web/img/gato.png") && text.contains(&reference), "{text}");
    }

    #[tokio::test]
    async fn unreadable_dimensions_show_question_mark() {
        let fx = Fixture::new("dims");
        let generator = Arc::new(FakeGenerator::new(|_| Ok(image(b"\x89PNG\r\n\x1a\ncurto".to_vec(), ImageFormat::Png))));
        let out = call(&fx.host(&generator, "T01", ""), gen("web/a.png", "p")).await.expect("ok");
        assert_eq!(out, "imagem gravada em web/a.png (?, 1 KB)");
    }

    #[tokio::test]
    async fn refusal_marks_unavailable_once_and_stops_calling() {
        let fx = Fixture::new("refusal");
        let generator = Arc::new(FakeGenerator::new(|_| Err(LlmError::Parse(IMAGE_UNSUPPORTED))));
        let host = fx.host(&generator, "T01", "");
        assert_eq!(call(&host, gen("web/a.png", "p")).await.expect("recusa"), IMAGE_UNAVAILABLE_MSG);
        // Mesmo com o gerador "disponível" de novo, a sessão não pede mais.
        generator.available.store(true, Ordering::SeqCst);
        assert_eq!(call(&host, gen("web/b.png", "q")).await.expect("depois"), IMAGE_UNAVAILABLE_MSG);
        assert_eq!(generator.calls(), 1);
        assert_eq!(generator.marks.load(Ordering::SeqCst), 1);
        assert!(fx.usage().is_empty() && images::read_manifest(&fx.spec_dir).is_empty());
        // Outra sessão com a capacidade marcada: indisponível sem pedido.
        generator.available.store(false, Ordering::SeqCst);
        assert_eq!(call(&fx.host(&generator, "T02", ""), gen("web/c.png", "r")).await.expect("x"), IMAGE_UNAVAILABLE_MSG);
        assert_eq!(generator.calls(), 1);
    }

    #[tokio::test]
    async fn other_provider_failures_show_only_the_fixed_code() {
        let fx = Fixture::new("fail");
        let generator = Arc::new(FakeGenerator::new(|_| {
            Err(LlmError::Http { status: 500, body_snippet: "texto remoto secreto".into(), code: None })
        }));
        let host = fx.host(&generator, "T01", "");
        let err = call(&host, gen("web/a.png", "p")).await.expect_err("falha");
        assert!(err.to_string().contains("500") && !err.to_string().contains("texto remoto"), "{err}");
        assert_eq!(generator.marks.load(Ordering::SeqCst), 0);
    }

    /// Spec 091: a falha final é relatada ao gerador (caminho + motivo fixo com o código saneado) e
    /// fica no host para a trilha; a resposta ao modelo segue a mesma. A recusa não é relatada aqui.
    #[tokio::test]
    async fn final_failure_is_reported_with_the_path_and_kept_for_the_trail() {
        let fx = Fixture::new("report");
        let generator = Arc::new(FakeGenerator::new(|_| {
            Err(LlmError::Http {
                status: 400,
                body_snippet: "texto remoto secreto".into(),
                code: crate::agents::llm_client::ProviderCode::parse("invalid_value"),
            })
        }));
        let host = fx.host(&generator, "T01", "");
        assert_eq!(host.take_image_failure(), None);
        let err = call(&host, gen("./web/img/a.png", "p")).await.expect_err("falha");
        assert_eq!(
            err.to_string(),
            "a geração da imagem falhou (provider_http_400 (código: invalid_value)); tente de novo mais tarde ou use SVG/CSS"
        );
        let failures = generator.failures.lock().expect("lock").clone();
        assert_eq!(failures, [("web/img/a.png".to_string(), "provider_http_400 (código: invalid_value)".to_string())]);
        assert_eq!(host.take_image_failure().as_deref(), Some("provider_http_400 (código: invalid_value)"));
        assert_eq!(host.take_image_failure(), None, "retirada uma vez");
        assert_eq!(host.last_image_failure().as_deref(), Some("provider_http_400 (código: invalid_value)"), "092 T03: fica para os FATOS");
        // Recusa de capacidade: só `mark_unsupported`, sem relato de falha.
        let refused = Arc::new(FakeGenerator::new(|_| Err(LlmError::Parse(IMAGE_UNSUPPORTED))));
        let host = fx.host(&refused, "T01", "");
        call(&host, gen("web/b.png", "p")).await.expect("recusa");
        assert!(refused.failures.lock().expect("lock").is_empty());
        assert_eq!(host.take_image_failure(), None);
    }

    /// Desfechos roteirizados do gerador falso, na ordem das chamadas.
    #[derive(Clone, Copy)]
    enum Step {
        Ok,
        Unreadable,
        WrongFormat,
        TooBig,
        Http500,
        Refused,
    }

    fn scripted(steps: Vec<Step>) -> Arc<FakeGenerator> {
        let queue = Mutex::new(std::collections::VecDeque::from(steps));
        Arc::new(FakeGenerator::new(move |_| {
            let step = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop_front().unwrap_or(Step::Ok);
            match step {
                Step::Ok => Ok(image(png(8, 8), ImageFormat::Png)),
                Step::Unreadable => Err(LlmError::Parse(crate::agents::llm_client::IMAGE_UNREADABLE)),
                Step::WrongFormat => Ok(image(webp(8, 8), ImageFormat::Webp)),
                Step::TooBig => {
                    let mut bytes = png(8, 8);
                    bytes.resize((MAX_IMAGE_BYTES + 1) as usize, 0);
                    Ok(image(bytes, ImageFormat::Png))
                }
                Step::Http500 => Err(LlmError::Http { status: 500, body_snippet: String::new(), code: None }),
                Step::Refused => Err(LlmError::Parse(IMAGE_UNSUPPORTED)),
            }
        }))
    }

    const UNREADABLE_REASON: &str = "provider_bad_response (o provedor não devolveu uma imagem legível (pode ter cobrado))";

    /// Spec 094 T01: 2 falhas cobradas seguidas suspendem a geração no resto da execução (o freio é
    /// do gerador: vale para outras sessões/tarefas), com aviso único; o rótulo próprio vai para a
    /// conversa, a trilha e os FATOS.
    #[tokio::test]
    async fn two_charged_failures_suspend_generation_for_the_execution() {
        let fx = Fixture::new("brake");
        let generator = scripted(vec![Step::Unreadable, Step::Unreadable, Step::Ok]);
        let host = fx.host(&generator, "T01", "");
        let err = call(&host, gen("web/a.png", "p1")).await.expect_err("ilegível");
        assert_eq!(
            err.to_string(),
            format!("a geração da imagem falhou ({UNREADABLE_REASON}); tente de novo mais tarde ou use SVG/CSS")
        );
        assert_eq!(host.take_image_failure().as_deref(), Some(UNREADABLE_REASON), "trilha com o rótulo próprio");
        assert!(!generator.suspended());
        let err = call(&host, gen("web/b.png", "p2")).await.expect_err("ilegível");
        assert_eq!(err.to_string(), format!("a geração da imagem falhou ({UNREADABLE_REASON}); {IMAGE_SUSPENDED_MSG}"));
        assert!(generator.suspended());
        let failures = generator.failures.lock().expect("lock").clone();
        assert_eq!(failures.len(), 2);
        assert!(failures.iter().all(|(_, r)| r == UNREADABLE_REASON), "{failures:?}");
        // Suspensa: sem pedido ao provedor, nem em outra sessão da mesma execução.
        assert_eq!(call(&host, gen("web/c.png", "p3")).await.expect("suspensa"), IMAGE_SUSPENDED_MSG);
        assert_eq!(host.last_image_failure().as_deref(), Some(REASON_SUSPENDED));
        let other = fx.host(&generator, "T02", "");
        assert_eq!(call(&other, gen("web/d.png", "p4")).await.expect("suspensa"), IMAGE_SUSPENDED_MSG);
        assert_eq!(generator.calls(), 2);
        // O aviso saiu uma vez só.
        assert_eq!(
            generator.suspension_notices.lock().expect("lock").as_slice(),
            [image_suspended_notice(2)]
        );
        assert!(fx.usage().is_empty() && images::read_manifest(&fx.spec_dir).is_empty());
    }

    #[tokio::test]
    async fn a_success_resets_the_charged_count() {
        let fx = Fixture::new("brake-reset");
        let generator = scripted(vec![Step::Unreadable, Step::Ok, Step::Unreadable, Step::Ok]);
        let host = fx.host(&generator, "T01", "");
        call(&host, gen("web/a.png", "p1")).await.expect_err("cobrada");
        call(&host, gen("web/b.png", "p2")).await.expect("sucesso");
        let err = call(&host, gen("web/c.png", "p3")).await.expect_err("cobrada");
        assert!(!err.to_string().contains(IMAGE_SUSPENDED_MSG), "{err}");
        assert!(!generator.suspended());
        call(&host, gen("web/d.png", "p4")).await.expect("segue gerando");
        assert_eq!(generator.calls(), 4);
        assert!(generator.suspension_notices.lock().expect("lock").is_empty());
    }

    /// Formato diferente do pedido e imagem acima de 4 MiB também são cobradas e não aproveitadas.
    #[tokio::test]
    async fn wrong_format_and_oversized_images_count_as_charged() {
        let fx = Fixture::new("brake-format");
        let generator = scripted(vec![Step::WrongFormat, Step::TooBig, Step::Ok]);
        let host = fx.host(&generator, "T01", "");
        let err = call(&host, gen("web/a.png", "p1")).await.expect_err("formato");
        assert!(err.to_string().contains("formato diferente") && !err.to_string().contains(IMAGE_SUSPENDED_MSG), "{err}");
        let err = call(&host, gen("web/b.png", "p2")).await.expect_err("grande");
        assert!(err.to_string().contains("4 MiB") && err.to_string().ends_with(IMAGE_SUSPENDED_MSG), "{err}");
        assert_eq!(call(&host, gen("web/c.png", "p3")).await.expect("suspensa"), IMAGE_SUSPENDED_MSG);
        assert_eq!(generator.calls(), 2);
        assert_eq!(generator.suspension_notices.lock().expect("lock").len(), 1);
        // As duas cobradas seguem no uso e no manifesto (contam no limite da spec).
        assert_eq!(fx.usage().len(), 2);
        assert_eq!(images::new_generations(&images::read_manifest(&fx.spec_dir)), 2);
    }

    /// Recusa e erros HTTP seguem como antes: não contam no freio (nem zeram).
    #[tokio::test]
    async fn refusal_and_http_errors_do_not_touch_the_brake() {
        let fx = Fixture::new("brake-http");
        let generator = scripted(vec![Step::Http500, Step::Http500, Step::Unreadable, Step::Http500, Step::Ok]);
        let host = fx.host(&generator, "T01", "");
        for path in ["web/a.png", "web/b.png", "web/c.png", "web/d.png"] {
            call(&host, gen(path, path)).await.expect_err("falha");
        }
        assert!(!generator.suspended(), "uma cobrada só");
        call(&host, gen("web/e.png", "e")).await.expect("imagem");
        assert_eq!(generator.calls(), 5);
        let refused = scripted(vec![Step::Refused]);
        let host = fx.host(&refused, "T01", "");
        assert_eq!(call(&host, gen("web/f.png", "f")).await.expect("recusa"), IMAGE_UNAVAILABLE_MSG);
        assert!(!refused.suspended() && refused.suspension_notices.lock().expect("lock").is_empty());
        assert_eq!(refused.marks.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn brake_suspends_once_and_reports_the_total() {
        let brake = ChargedBrake::default();
        assert_eq!(brake.charged(), None);
        brake.success();
        assert_eq!(brake.charged(), None);
        assert_eq!(brake.charged(), Some(3), "total de cobradas na execução");
        assert!(brake.suspended());
        assert_eq!(brake.charged(), None, "suspende uma vez só");
        brake.success();
        assert!(brake.suspended(), "o sucesso não levanta a suspensão");
        assert_eq!(
            image_suspended_notice(2),
            "O provedor respondeu sem uma imagem que o Nants consiga ler (2 tentativas); a geração pode ter sido \
             cobrada pelo provedor. As imagens restantes seguem pelo fallback do plano."
        );
    }

    #[tokio::test]
    async fn without_link_or_for_other_roles_the_tool_is_unavailable() {
        let fx = Fixture::new("nolink");
        let plain = ToolHost::new(Role::Developer, fx.root.clone(), vec!["web/**".into()], Policy::new(Vec::new(), Vec::new()), Arc::new(Deny));
        assert!(matches!(call(&plain, gen("web/a.png", "p")).await, Err(ToolError::Denied(_))));
        let reviewer = ToolHost::new(Role::Reviewer, fx.root.clone(), Vec::new(), Policy::new(Vec::new(), Vec::new()), Arc::new(Deny));
        assert!(matches!(call(&reviewer, gen("web/a.png", "p")).await, Err(ToolError::NotPermitted { .. })));
        assert!(!crate::harness::tools::specs(Role::Developer).iter().any(|s| s.name == "generate_image"));
        assert!(!Path::new(&fx.root.join("web/a.png")).exists());
    }
}
