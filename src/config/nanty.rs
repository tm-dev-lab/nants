//! Seção `[nanty]` da configuração (spec 138 T01, C-04): o agente de nível de sistema.
//!
//! Fica fora de [`super::Agents`] de propósito: o `PUT /api/config/agents` substitui o bloco dos
//! agentes inteiro e apagaria o Nanty se ele morasse lá.
//!
//! **Valores iniciais provisórios** (decisão do Arquiteto na spec 138; ajustáveis aqui ou nos
//! Ajustes da janela): orçamento diário de US$ 0,50, silêncio das 22:00 às 07:00, resumo às 08:00,
//! voz desligada, Telegram sem bot e nenhum projeto excluído (projeto novo já nasce observado).
//!
//! **Compatibilidade:** a seção só é gravada fora do padrão (`skip_serializing_if`). Uma versão
//! anterior do Nants lê a configuração e ignora a seção, mas, se regravar o arquivo (salvar uma
//! conexão, por exemplo), a seção é descartada: o Nanty volta aos valores iniciais e o bot precisa
//! ser informado e pareado de novo.

use std::collections::BTreeMap;
use std::env;

use serde::{Deserialize, Serialize};

use super::schema::Effort;

/// Orçamento diário inicial do modelo do Nanty, em US$ (provisório).
pub const DEFAULT_DAILY_BUDGET_USD: f64 = 0.5;
/// Início do silêncio inicial (provisório).
pub const DEFAULT_QUIET_START: &str = "22:00";
/// Fim do silêncio inicial (provisório).
pub const DEFAULT_QUIET_END: &str = "07:00";
/// Hora inicial do resumo da manhã (provisório).
pub const DEFAULT_DIGEST_TIME: &str = "08:00";
/// Orçamento diário em tokens quando o modelo do Nanty não tem preço (ou vai pelo plano), somando
/// entrada e saída (provisório, spec 138 T05).
pub const DEFAULT_DAILY_BUDGET_TOKENS: u64 = 200_000;

/// Configuração do Nanty (`[nanty]`). Tudo opcional no TOML.
///
/// Cada campo só é gravado quando difere do padrão: o arquivo guarda só o que a pessoa mudou, e uma
/// mudança futura dos valores provisórios no código chega a quem já tem a seção.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyConfig {
    /// Conexão do modelo do Nanty; vazia = sem modelo (os avisos seguem com o texto fixo).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub connection_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "is_default_effort")]
    pub effort: Effort,
    /// Orçamento diário do modelo e da voz, em US$. Modelo sem preço conta tokens (T05/T11).
    #[serde(skip_serializing_if = "is_default_budget")]
    pub daily_budget_usd: f64,
    /// Orçamento diário em tokens (entrada mais saída) para o modelo sem preço ou pelo plano (T05).
    #[serde(skip_serializing_if = "is_default_budget_tokens")]
    pub daily_budget_tokens: u64,
    /// Horário de silêncio (`HH:MM`, hora local); início igual ao fim = sem silêncio.
    #[serde(skip_serializing_if = "is_default_quiet_start")]
    pub quiet_start: String,
    #[serde(skip_serializing_if = "is_default_quiet_end")]
    pub quiet_end: String,
    /// Hora local do resumo da manhã (`HH:MM`).
    #[serde(skip_serializing_if = "is_default_digest")]
    pub digest_time: String,
    /// Resumo da manhã num dia sem nada a destacar: uma mensagem curta ou nenhuma (T05).
    #[serde(skip_serializing_if = "DigestEmpty::is_default")]
    pub digest_empty: DigestEmpty,
    /// Idioma das mensagens do Nanty (`pt-BR` ou `en`), atualizado quando a interface troca
    /// (`server::lang`). Ausente = pt-BR até a interface informar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Projetos que o Nanty não observa (o resto, inclusive projeto novo, é observado).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded_projects: Vec<String>,
    /// Nível por id do catálogo de ações (T03); ausente = o padrão do catálogo.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub actions: BTreeMap<String, ActionLevel>,
    /// Reserva por papel (`architect`, `developer`, `reviewer`) para a troca quando o crédito acaba.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub reserve: BTreeMap<String, NantyRoute>,
    #[serde(skip_serializing_if = "NantyVoice::is_default")]
    pub voice: NantyVoice,
    #[serde(skip_serializing_if = "NantyTelegram::is_default")]
    pub telegram: NantyTelegram,
    /// Urgência por tipo de aviso (spec 138 T02): só o que a pessoa mudou; o resto vem da tabela
    /// fixa do código. O modelo nunca decide a urgência.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub urgency: BTreeMap<String, Urgency>,
    /// Tempos e limiares do filtro dos avisos (spec 138 T02, provisórios).
    #[serde(skip_serializing_if = "NantyFilter::is_default")]
    pub filter: NantyFilter,
}

impl Default for NantyConfig {
    fn default() -> Self {
        Self {
            connection_id: String::new(),
            model: String::new(),
            effort: DEFAULT_EFFORT,
            daily_budget_usd: DEFAULT_DAILY_BUDGET_USD,
            daily_budget_tokens: DEFAULT_DAILY_BUDGET_TOKENS,
            quiet_start: DEFAULT_QUIET_START.to_string(),
            quiet_end: DEFAULT_QUIET_END.to_string(),
            digest_time: DEFAULT_DIGEST_TIME.to_string(),
            digest_empty: DigestEmpty::default(),
            lang: None,
            excluded_projects: Vec::new(),
            actions: BTreeMap::new(),
            reserve: BTreeMap::new(),
            voice: NantyVoice::default(),
            telegram: NantyTelegram::default(),
            urgency: BTreeMap::new(),
            filter: NantyFilter::default(),
        }
    }
}

/// Esforço inicial do modelo do Nanty.
const DEFAULT_EFFORT: Effort = Effort::Low;

fn is_default_effort(v: &Effort) -> bool {
    *v == DEFAULT_EFFORT
}

fn is_default_budget(v: &f64) -> bool {
    *v == DEFAULT_DAILY_BUDGET_USD
}

fn is_default_budget_tokens(v: &u64) -> bool {
    *v == DEFAULT_DAILY_BUDGET_TOKENS
}

fn is_default_quiet_start(v: &str) -> bool {
    v == DEFAULT_QUIET_START
}

fn is_default_quiet_end(v: &str) -> bool {
    v == DEFAULT_QUIET_END
}

fn is_default_digest(v: &str) -> bool {
    v == DEFAULT_DIGEST_TIME
}

/// `Debug` manual: o token do bot nunca aparece.
impl std::fmt::Debug for NantyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NantyConfig")
            .field("connection_id", &self.connection_id)
            .field("model", &self.model)
            .field("effort", &self.effort)
            .field("daily_budget_usd", &self.daily_budget_usd)
            .field("daily_budget_tokens", &self.daily_budget_tokens)
            .field("quiet", &(&self.quiet_start, &self.quiet_end))
            .field("digest_time", &self.digest_time)
            .field("digest_empty", &self.digest_empty)
            .field("lang", &self.lang)
            .field("excluded_projects", &self.excluded_projects)
            .field("actions", &self.actions)
            .field("reserve", &self.reserve)
            .field("voice", &self.voice)
            .field("telegram", &self.telegram)
            .field("urgency", &self.urgency)
            .field("filter", &self.filter)
            .finish()
    }
}

impl NantyConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// O projeto é observado (não está entre os excluídos).
    pub fn observes(&self, project_id: &str) -> bool {
        !self.excluded_projects.iter().any(|p| p == project_id)
    }

    /// Orçamento efetivo: o configurado, ou zero se for inválido (negativo ou não finito).
    pub fn budget_usd(&self) -> f64 {
        Some(self.daily_budget_usd).filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0)
    }

    /// Silêncio em minutos do dia `(início, fim)`; horário inválido cai no padrão.
    pub fn quiet_window(&self) -> (u16, u16) {
        (
            parse_hhmm(&self.quiet_start).or_else(|| parse_hhmm(DEFAULT_QUIET_START)).unwrap_or(0),
            parse_hhmm(&self.quiet_end).or_else(|| parse_hhmm(DEFAULT_QUIET_END)).unwrap_or(0),
        )
    }

    /// Hora do resumo em minutos do dia; inválida cai no padrão.
    pub fn digest_minute(&self) -> u16 {
        parse_hhmm(&self.digest_time).or_else(|| parse_hhmm(DEFAULT_DIGEST_TIME)).unwrap_or(0)
    }

    /// Pares `(conexão, modelo)` que o Nanty pode usar: o dele, os da voz e as reservas. Entram no
    /// `models_in_use`, para o preço ser buscado.
    pub fn routes(&self) -> Vec<(&str, &str)> {
        let mut out = vec![(self.connection_id.as_str(), self.model.as_str())];
        out.extend(self.voice.order.iter().map(|r| (r.connection_id.as_str(), r.model.as_str())));
        out.push((self.voice.transcribe.connection_id.as_str(), self.voice.transcribe.model.as_str()));
        out.extend(self.reserve.values().map(|r| (r.connection_id.as_str(), r.model.as_str())));
        out
    }
}

/// O resumo da manhã num dia sem nada a destacar (T05).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestEmpty {
    /// Uma mensagem curta, só com o número de projetos observados.
    #[default]
    Short,
    /// Nenhuma mensagem.
    Skip,
}

impl DigestEmpty {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `HH:MM` (00:00 a 23:59) em minutos do dia.
pub fn parse_hhmm(raw: &str) -> Option<u16> {
    let (h, m) = raw.trim().split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    let (h, m): (u16, u16) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

/// Urgência de um aviso (spec 138 T02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    /// Vai na hora; no horário de silêncio, vai sem som.
    Urgent,
    /// Vai na hora; no horário de silêncio, vai para o resumo.
    Normal,
    /// Só no resumo.
    Digest,
}

/// Espera antes do primeiro aviso de um grupo (provisório), em segundos.
pub const DEFAULT_GROUP_WAIT_SECS: u64 = 30;
/// Intervalo do grupo: espera mínima para avisar de novo quando os fatos mudam (provisório).
pub const DEFAULT_GROUP_INTERVAL_SECS: u64 = 5 * 60;
/// Intervalo de repetição do mesmo aviso, sem mudança (provisório).
pub const DEFAULT_REPEAT_INTERVAL_SECS: u64 = 4 * 60 * 60;
/// Varredura periódica dos observadores (provisório).
pub const DEFAULT_SCAN_INTERVAL_SECS: u64 = 60;
/// Espaço livre mínimo nas pastas do Nants (o mesmo do `doctor`), em GB (provisório).
pub const DEFAULT_DISK_MIN_FREE_GB: u64 = 5;
/// Espaço livre mínimo na pasta temporária, em GB (provisório).
pub const DEFAULT_TMP_MIN_FREE_GB: u64 = 5;
/// Menor varredura aceita: um valor menor (ou zero) cai aqui.
pub const MIN_SCAN_INTERVAL_SECS: u64 = 10;

/// Filtro dos avisos no modelo do Alertmanager (spec 138 T02). Valores provisórios; cada campo só é
/// gravado quando difere do padrão.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyFilter {
    #[serde(skip_serializing_if = "is_default_group_wait")]
    pub group_wait_secs: u64,
    #[serde(skip_serializing_if = "is_default_group_interval")]
    pub group_interval_secs: u64,
    #[serde(skip_serializing_if = "is_default_repeat_interval")]
    pub repeat_interval_secs: u64,
    #[serde(skip_serializing_if = "is_default_scan_interval")]
    pub scan_interval_secs: u64,
    #[serde(skip_serializing_if = "is_default_disk_min")]
    pub disk_min_free_gb: u64,
    #[serde(skip_serializing_if = "is_default_tmp_min")]
    pub tmp_min_free_gb: u64,
}

impl Default for NantyFilter {
    fn default() -> Self {
        Self {
            group_wait_secs: DEFAULT_GROUP_WAIT_SECS,
            group_interval_secs: DEFAULT_GROUP_INTERVAL_SECS,
            repeat_interval_secs: DEFAULT_REPEAT_INTERVAL_SECS,
            scan_interval_secs: DEFAULT_SCAN_INTERVAL_SECS,
            disk_min_free_gb: DEFAULT_DISK_MIN_FREE_GB,
            tmp_min_free_gb: DEFAULT_TMP_MIN_FREE_GB,
        }
    }
}

fn is_default_group_wait(v: &u64) -> bool {
    *v == DEFAULT_GROUP_WAIT_SECS
}

fn is_default_group_interval(v: &u64) -> bool {
    *v == DEFAULT_GROUP_INTERVAL_SECS
}

fn is_default_repeat_interval(v: &u64) -> bool {
    *v == DEFAULT_REPEAT_INTERVAL_SECS
}

fn is_default_scan_interval(v: &u64) -> bool {
    *v == DEFAULT_SCAN_INTERVAL_SECS
}

fn is_default_disk_min(v: &u64) -> bool {
    *v == DEFAULT_DISK_MIN_FREE_GB
}

fn is_default_tmp_min(v: &u64) -> bool {
    *v == DEFAULT_TMP_MIN_FREE_GB
}

/// 1 GB na base 1024, como o `doctor` (`MIN_FREE_BYTES`) e o `format_gb` contam.
pub const GB_BYTES: u64 = 1024 * 1024 * 1024;

impl NantyFilter {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn group_wait_ms(&self) -> u64 {
        self.group_wait_secs.saturating_mul(1000)
    }

    pub fn group_interval_ms(&self) -> u64 {
        self.group_interval_secs.saturating_mul(1000)
    }

    pub fn repeat_interval_ms(&self) -> u64 {
        self.repeat_interval_secs.saturating_mul(1000)
    }

    /// Intervalo da varredura, nunca abaixo de [`MIN_SCAN_INTERVAL_SECS`].
    pub fn scan_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.scan_interval_secs.max(MIN_SCAN_INTERVAL_SECS))
    }

    pub fn disk_min_free_bytes(&self) -> u64 {
        self.disk_min_free_gb.saturating_mul(GB_BYTES)
    }

    pub fn tmp_min_free_bytes(&self) -> u64 {
        self.tmp_min_free_gb.saturating_mul(GB_BYTES)
    }
}

/// Nível de uma ação do catálogo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionLevel {
    /// Desligada: nem sugere.
    Off,
    /// Pergunta antes (cartão de aprovação).
    Ask,
    /// Automática.
    Auto,
}

/// Uma conexão e um modelo (reserva por papel ou passo da voz).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyRoute {
    pub connection_id: String,
    pub model: String,
}

/// Modelo de voz inicial de uma conexão do Google Gemini (provisório, spec 138 T09): o TTS da
/// Interactions API, o mesmo que o vídeo do Nants usa.
pub const DEFAULT_GEMINI_TTS_MODEL: &str = "gemini-3.8-flash-tts";
/// Voz inicial do Gemini (provisória).
pub const DEFAULT_GEMINI_VOICE: &str = "Kore";
/// Modelo de voz inicial de uma conexão da OpenAI (provisório): o do `/v1/audio/speech`.
pub const DEFAULT_OPENAI_TTS_MODEL: &str = "gpt-4o-mini-tts-2025-12-15";
/// Voz inicial da OpenAI (provisória).
pub const DEFAULT_OPENAI_VOICE: &str = "marin";
/// Modelo inicial da transcrição das notas de voz, numa conexão do Google Gemini (provisório).
pub const DEFAULT_TRANSCRIBE_MODEL: &str = "gemini-3.8-flash";
/// Nota de voz mais longa aceita para transcrição, em segundos (provisório).
pub const DEFAULT_VOICE_MAX_INPUT_SECS: u64 = 300;

/// Um passo da voz: a conexão e, opcionalmente, o modelo e a voz (vazios = os iniciais do tipo
/// da conexão).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceRoute {
    pub connection_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub voice: String,
}

/// Voz do Nanty (T09): ligada, a ordem das conexões da fala, a conexão da transcrição e o teto da
/// nota de voz recebida.
///
/// - **`order` vazia = automática:** as conexões do Google Gemini com chave, depois as da OpenAI
///   (`api.openai.com`), e por fim o texto. Com a lista, vale a ordem dela; o texto é sempre o
///   último passo.
/// - **`transcribe` vazia = automática:** a primeira conexão do Google Gemini com chave, com o
///   modelo inicial.
/// - Cada campo só é gravado quando difere do padrão.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyVoice {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<VoiceRoute>,
    #[serde(skip_serializing_if = "is_empty_route")]
    pub transcribe: NantyRoute,
    #[serde(skip_serializing_if = "is_default_max_input")]
    pub max_input_secs: u64,
}

impl Default for NantyVoice {
    fn default() -> Self {
        Self { enabled: false, order: Vec::new(), transcribe: NantyRoute::default(), max_input_secs: DEFAULT_VOICE_MAX_INPUT_SECS }
    }
}

fn is_empty_route(r: &NantyRoute) -> bool {
    r.connection_id.is_empty() && r.model.is_empty()
}

fn is_default_max_input(v: &u64) -> bool {
    *v == DEFAULT_VOICE_MAX_INPUT_SECS
}

impl NantyVoice {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Espera do envio ao Telegram enquanto há uma aba do Nants visível (C-07, T07), em segundos
/// (provisório).
pub const DEFAULT_VISIBLE_HOLD_SECS: u64 = 3 * 60;

/// Telegram do Nanty (T06): token do bot, chat do dono pareado e o offset do long polling. A espera
/// com aba visível (T07, C-07) vem ligada e só é gravada se a pessoa mudar.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyTelegram {
    /// Valor literal ou `env:NOME`, como as chaves de API. Nunca sai do servidor (só a máscara).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bot_token: String,
    /// Chat privado do dono, depois do pareamento.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<i64>,
    /// `from.id` do dono, fixado no pareamento (T06, C-11): mensagem ou clique de outro id é
    /// ignorado e registrado.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<i64>,
    /// Próximo `update_id` a pedir (gravado depois de processar, C-10).
    #[serde(skip_serializing_if = "is_zero")]
    pub offset: i64,
    /// Com uma aba do Nants visível, o envio ao Telegram espera [`Self::visible_hold_secs`]; o que
    /// for decidido na janela nesse meio não vai (C-07).
    #[serde(skip_serializing_if = "is_true")]
    pub hold_while_visible: bool,
    #[serde(skip_serializing_if = "is_default_visible_hold")]
    pub visible_hold_secs: u64,
}

impl Default for NantyTelegram {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            chat_id: None,
            owner_id: None,
            offset: 0,
            hold_while_visible: true,
            visible_hold_secs: DEFAULT_VISIBLE_HOLD_SECS,
        }
    }
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_default_visible_hold(v: &u64) -> bool {
    *v == DEFAULT_VISIBLE_HOLD_SECS
}

impl NantyTelegram {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Resolve o token: `env:NOME` lê a variável (`None` se ausente ou vazia); senão o literal
    /// (`None` se vazio).
    pub fn resolve_token(&self) -> Option<String> {
        let value = match self.bot_token.strip_prefix("env:") {
            Some(name) => env::var(name).ok()?,
            None => self.bot_token.clone(),
        };
        Some(value).filter(|v| !v.trim().is_empty())
    }

    /// Há um dono pareado: o chat e o `from.id` fixados no pareamento (T06, C-11). O mesmo critério
    /// no `/api/nanty/status`, no `/api/nanty/config` e no `/api/nanty/telegram` (T11).
    pub fn is_paired(&self) -> bool {
        self.chat_id.is_some() && self.owner_id.is_some()
    }

    /// Espera com aba visível, em ms; desligada = zero.
    pub fn visible_hold_ms(&self) -> u64 {
        if self.hold_while_visible {
            self.visible_hold_secs.saturating_mul(1000)
        } else {
            0
        }
    }
}

/// `Debug` manual: redige o token.
impl std::fmt::Debug for NantyTelegram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NantyTelegram")
            .field("bot_token", &if self.bot_token.is_empty() { "" } else { "***" })
            .field("chat_id", &self.chat_id.map(|_| "***"))
            .field("owner_id", &self.owner_id.map(|_| "***"))
            .field("offset", &self.offset)
            .field("hold_while_visible", &self.hold_while_visible)
            .field("visible_hold_secs", &self.visible_hold_secs)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn default_is_not_written_and_old_config_loads() {
        let text = toml::to_string(&Config::default()).expect("ser");
        assert!(!text.contains("nanty"), "{text}");
        let cfg: Config = toml::from_str("[server]\nport = 3000\n").expect("antiga");
        assert!(cfg.nanty.is_default());
        assert_eq!(cfg.nanty.quiet_window(), (22 * 60, 7 * 60));
        assert_eq!(cfg.nanty.digest_minute(), 8 * 60);
        assert!((cfg.nanty.budget_usd() - DEFAULT_DAILY_BUDGET_USD).abs() < f64::EPSILON);
        assert!(cfg.nanty.observes("qualquer"), "projeto novo nasce observado");
    }

    #[test]
    fn full_section_round_trips() {
        let raw = r#"
[nanty]
connection_id = "c1"
model = "m-nanty"
effort = "medium"
daily_budget_usd = 1.25
quiet_start = "23:30"
quiet_end = "06:15"
digest_time = "09:00"
digest_empty = "skip"
daily_budget_tokens = 5000
lang = "en"
excluded_projects = ["p2"]

[nanty.actions]
retry_provider = "auto"
approve_plan = "ask"
docker_cleanup = "off"

[nanty.reserve.developer]
connection_id = "c2"
model = "m-reserva"

[nanty.voice]
enabled = true
order = [{ connection_id = "g", model = "m-voz-g" }, { connection_id = "o", model = "m-voz-o" }]

[nanty.telegram]
bot_token = "env:NANTY_BOT"
chat_id = 42
offset = 7
"#;
        let cfg: Config = toml::from_str(raw).expect("nanty");
        let n = &cfg.nanty;
        assert_eq!((n.connection_id.as_str(), n.model.as_str(), n.effort), ("c1", "m-nanty", Effort::Medium));
        assert_eq!(n.quiet_window(), (23 * 60 + 30, 6 * 60 + 15));
        assert_eq!(n.lang.as_deref(), Some("en"));
        assert_eq!((n.digest_empty, n.daily_budget_tokens), (DigestEmpty::Skip, 5000));
        assert!(!n.observes("p2") && n.observes("p1"));
        assert_eq!(n.actions["retry_provider"], ActionLevel::Auto);
        assert_eq!(n.reserve["developer"].model, "m-reserva");
        assert_eq!(n.voice.order.len(), 2);
        assert_eq!((n.telegram.chat_id, n.telegram.offset), (Some(42), 7));
        let routes: Vec<&str> = n.routes().into_iter().map(|(_, m)| m).collect();
        assert_eq!(routes, ["m-nanty", "m-voz-g", "m-voz-o", "", "m-reserva"]);
        let back: Config = toml::from_str(&toml::to_string(&cfg).expect("ser")).expect("de");
        assert_eq!(back.nanty, cfg.nanty);
    }

    /// Só o que a pessoa mudou vai para o arquivo; os valores provisórios ficam no código.
    #[test]
    fn only_changed_fields_are_written() {
        let mut cfg = Config::default();
        cfg.nanty.lang = Some("en".into());
        let text = toml::to_string(&cfg).expect("ser");
        let section = text.split("[nanty]").nth(1).expect("seção").trim();
        assert_eq!(section, "lang = \"en\"", "{text}");
        cfg.nanty.daily_budget_usd = 2.0;
        cfg.nanty.quiet_end = "06:00".into();
        let text = toml::to_string(&cfg).expect("ser");
        let section = text.split("[nanty]").nth(1).expect("seção");
        for absent in ["quiet_start", "digest_time", "digest_empty", "daily_budget_tokens", "effort", "model", "connection_id", "voice", "telegram"] {
            assert!(!section.contains(absent), "{absent} no arquivo: {text}");
        }
        assert!(section.contains("daily_budget_usd = 2.0") && section.contains("quiet_end = \"06:00\""), "{text}");
        let back: Config = toml::from_str(&text).expect("de");
        assert_eq!(back.nanty, cfg.nanty);
    }

    /// A espera com aba visível (T07, C-07) vem ligada e só vai para o arquivo se mudar.
    #[test]
    fn visible_hold_is_on_by_default_and_written_only_when_changed() {
        let tg = NantyTelegram::default();
        assert!(tg.hold_while_visible);
        assert_eq!(tg.visible_hold_ms(), DEFAULT_VISIBLE_HOLD_SECS * 1000);
        let mut cfg = Config::default();
        cfg.nanty.telegram.chat_id = Some(5);
        let text = toml::to_string(&cfg).expect("ser");
        assert!(!text.contains("hold_while_visible") && !text.contains("visible_hold_secs"), "{text}");
        cfg.nanty.telegram.hold_while_visible = false;
        cfg.nanty.telegram.visible_hold_secs = 60;
        let text = toml::to_string(&cfg).expect("ser");
        assert!(text.contains("hold_while_visible = false") && text.contains("visible_hold_secs = 60"), "{text}");
        assert_eq!(cfg.nanty.telegram.visible_hold_ms(), 0, "desligada não espera");
        let back: Config = toml::from_str(&text).expect("de");
        assert_eq!(back.nanty.telegram, cfg.nanty.telegram);
    }

    #[test]
    fn invalid_times_and_budget_fall_back() {
        let n = NantyConfig {
            quiet_start: "25:00".into(),
            quiet_end: "7h".into(),
            digest_time: "".into(),
            daily_budget_usd: -1.0,
            ..NantyConfig::default()
        };
        assert_eq!(n.quiet_window(), (22 * 60, 7 * 60));
        assert_eq!(n.digest_minute(), 8 * 60);
        assert_eq!(n.budget_usd(), 0.0);
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(23 * 60 + 59));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("1:00"), None);
    }

    #[test]
    fn token_accepts_env_and_never_shows_in_debug() {
        let literal = format!("{}:{}", 123_456, "AAH-segredo-do-bot");
        let tg = NantyTelegram { bot_token: literal.clone(), chat_id: Some(99), owner_id: Some(98), ..NantyTelegram::default() };
        assert_eq!(tg.resolve_token().as_deref(), Some(literal.as_str()));
        let cfg = NantyConfig { telegram: tg, ..NantyConfig::default() };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("segredo") && !dbg.contains("99") && !dbg.contains("98"), "{dbg}");
        let var = "NANTS_TEST_138_T01_BOT";
        env::set_var(var, "valor-do-bot");
        let tg = NantyTelegram { bot_token: format!("env:{var}"), ..NantyTelegram::default() };
        assert_eq!(tg.resolve_token().as_deref(), Some("valor-do-bot"));
        env::remove_var(var);
        assert_eq!(tg.resolve_token(), None);
        assert_eq!(NantyTelegram::default().resolve_token(), None);
    }
}
