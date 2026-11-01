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
    /// Idioma da interface lembrado (`pt-BR` ou `en`), atualizado quando a interface troca
    /// (`server::lang`). Ausente = pt-BR até a interface informar. É a última reserva do idioma do
    /// Nanty (ver [`NantyLanguage`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Idioma próprio do Nanty para os avisos e o resumo (spec 139 T05, C-13): seguir a conversa
    /// (o padrão), português ou inglês. A conversa responde sempre no idioma de quem falou.
    #[serde(skip_serializing_if = "NantyLanguage::is_default")]
    pub language: NantyLanguage,
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
    /// Limites do laço da conversa (spec 139 T03, provisórios).
    #[serde(skip_serializing_if = "NantyChat::is_default")]
    pub chat: NantyChat,
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
            language: NantyLanguage::default(),
            excluded_projects: Vec::new(),
            actions: BTreeMap::new(),
            reserve: BTreeMap::new(),
            voice: NantyVoice::default(),
            telegram: NantyTelegram::default(),
            urgency: BTreeMap::new(),
            filter: NantyFilter::default(),
            chat: NantyChat::default(),
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
            .field("chat", &self.chat)
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

    /// Pares `(conexão, modelo)` que o Nanty pode usar: o dele e as reservas. Entram no
    /// `models_in_use`, para o preço ser buscado. Os da voz entram lá à parte, os efetivos e só com
    /// a voz ligada (`config_api::nanty_voice_models`).
    pub fn routes(&self) -> Vec<(&str, &str)> {
        let mut out = vec![(self.connection_id.as_str(), self.model.as_str())];
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

/// O idioma próprio do Nanty (spec 139 T05): `follow` segue o último idioma em que a pessoa falou
/// (e, sem ele, o da interface); `pt-BR` e `en` fixam o idioma.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NantyLanguage {
    #[default]
    #[serde(rename = "follow")]
    Follow,
    #[serde(rename = "pt-BR")]
    PtBr,
    #[serde(rename = "en")]
    En,
}

impl NantyLanguage {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// O valor da configuração e da API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Follow => "follow",
            Self::PtBr => "pt-BR",
            Self::En => "en",
        }
    }

    /// Lê o valor da API; qualquer outro é `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        [Self::Follow, Self::PtBr, Self::En].into_iter().find(|v| v.as_str() == raw.trim())
    }

    /// O idioma fixo escolhido; `None` com `follow`.
    pub fn fixed(self) -> Option<crate::i18n::Lang> {
        match self {
            Self::Follow => None,
            Self::PtBr => Some(crate::i18n::Lang::PtBr),
            Self::En => Some(crate::i18n::Lang::En),
        }
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

/// Rodadas do modelo por mensagem da conversa, contando a última, sem ferramentas (provisório).
pub const DEFAULT_CHAT_MAX_ROUNDS: u32 = 5;
/// Prazo total de uma resposta da conversa, em segundos (provisório).
pub const DEFAULT_CHAT_DEADLINE_SECS: u64 = 120;
/// Chamadas de ferramenta atendidas numa resposta do modelo; as outras recebem o aviso do limite
/// (provisório).
pub const DEFAULT_CHAT_MAX_CALLS: u32 = 4;
/// Tokens (entrada mais saída) de todas as rodadas de uma mensagem; passou disso, a rodada seguinte
/// é a última, sem ferramentas (provisório).
pub const DEFAULT_CHAT_MAX_TOKENS: u64 = 60_000;
/// Mensagens da conversa levadas como memória (provisório).
pub const DEFAULT_CHAT_MEMORY_MESSAGES: u32 = 12;
/// Janela da memória, em minutos: mensagem mais velha que isto fica de fora (provisório).
pub const DEFAULT_CHAT_MEMORY_WINDOW_MINS: u64 = 180;
/// Leituras de `project_details` (dado não confiável) atendidas numa mensagem; as outras recebem o
/// aviso do limite (spec 139 T04, C-07; provisório).
pub const DEFAULT_CHAT_MAX_DETAILS: u32 = 2;

/// Limites do laço de agente da conversa (spec 139 T03, C-02, C-06 e C-10). Valores provisórios;
/// cada campo só é gravado quando difere do padrão, e os acessores põem o valor numa faixa segura.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyChat {
    #[serde(skip_serializing_if = "is_default_chat_rounds")]
    pub max_rounds: u32,
    #[serde(skip_serializing_if = "is_default_chat_deadline")]
    pub deadline_secs: u64,
    #[serde(skip_serializing_if = "is_default_chat_calls")]
    pub max_calls_per_reply: u32,
    #[serde(skip_serializing_if = "is_default_chat_tokens")]
    pub max_tokens_per_message: u64,
    #[serde(skip_serializing_if = "is_default_chat_memory")]
    pub memory_messages: u32,
    #[serde(skip_serializing_if = "is_default_chat_window")]
    pub memory_window_mins: u64,
    #[serde(skip_serializing_if = "is_default_chat_details")]
    pub max_details_per_turn: u32,
}

impl Default for NantyChat {
    fn default() -> Self {
        Self {
            max_rounds: DEFAULT_CHAT_MAX_ROUNDS,
            deadline_secs: DEFAULT_CHAT_DEADLINE_SECS,
            max_calls_per_reply: DEFAULT_CHAT_MAX_CALLS,
            max_tokens_per_message: DEFAULT_CHAT_MAX_TOKENS,
            memory_messages: DEFAULT_CHAT_MEMORY_MESSAGES,
            memory_window_mins: DEFAULT_CHAT_MEMORY_WINDOW_MINS,
            max_details_per_turn: DEFAULT_CHAT_MAX_DETAILS,
        }
    }
}

fn is_default_chat_rounds(v: &u32) -> bool {
    *v == DEFAULT_CHAT_MAX_ROUNDS
}

fn is_default_chat_deadline(v: &u64) -> bool {
    *v == DEFAULT_CHAT_DEADLINE_SECS
}

fn is_default_chat_calls(v: &u32) -> bool {
    *v == DEFAULT_CHAT_MAX_CALLS
}

fn is_default_chat_tokens(v: &u64) -> bool {
    *v == DEFAULT_CHAT_MAX_TOKENS
}

fn is_default_chat_memory(v: &u32) -> bool {
    *v == DEFAULT_CHAT_MEMORY_MESSAGES
}

fn is_default_chat_window(v: &u64) -> bool {
    *v == DEFAULT_CHAT_MEMORY_WINDOW_MINS
}

fn is_default_chat_details(v: &u32) -> bool {
    *v == DEFAULT_CHAT_MAX_DETAILS
}

impl NantyChat {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Rodadas por mensagem, de 1 a 20 (a última vai sem ferramentas).
    pub fn rounds(&self) -> usize {
        self.max_rounds.clamp(1, 20) as usize
    }

    /// Prazo total da resposta, de 1 s a 10 min.
    pub fn deadline(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.deadline_secs.clamp(1, 600))
    }

    /// Chamadas atendidas por resposta, de 1 a 16.
    pub fn calls(&self) -> usize {
        self.max_calls_per_reply.clamp(1, 16) as usize
    }

    /// Teto de tokens por mensagem, no mínimo 1.000.
    pub fn tokens(&self) -> u64 {
        self.max_tokens_per_message.max(1_000)
    }

    /// Mensagens da memória, até 50.
    pub fn memory(&self) -> usize {
        self.memory_messages.min(50) as usize
    }

    /// Janela da memória, em ms.
    pub fn memory_window_ms(&self) -> u64 {
        self.memory_window_mins.saturating_mul(60_000)
    }

    /// Leituras de `project_details` por mensagem, de 0 a 8 (0: nenhuma).
    pub fn details(&self) -> usize {
        self.max_details_per_turn.min(8) as usize
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

/// Modelo de fala inicial do Nanty (provisório, spec 138 T09 e 139 T07): o TTS da Interactions API
/// do Gemini, o mesmo que o vídeo do Nants usa. O `gemini-3.8-flash-lite-tts` é só uma opção que a
/// pessoa escolhe; o Nants nunca troca de modelo sozinho.
pub const DEFAULT_GEMINI_TTS_MODEL: &str = "gemini-3.8-flash-tts";
/// Voz inicial do Gemini (provisória; a lista vem do `/v1beta/voices`).
pub const DEFAULT_GEMINI_VOICE: &str = "Kore";
/// Modelo inicial da transcrição das notas de voz, na mesma conexão do Gemini (provisório).
pub const DEFAULT_TRANSCRIBE_MODEL: &str = "gemini-3.8-flash";
/// Nota de voz mais longa aceita para transcrição, em segundos (provisório).
pub const DEFAULT_VOICE_MAX_INPUT_SECS: u64 = 300;

/// Um passo da ordem antiga da voz (até a rc.22): só lido, para a migração ([`NantyVoice::migrate`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceRoute {
    pub connection_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub voice: String,
}

/// O que a migração da forma antiga da voz fez (spec 139 T07, C-16), para a linha do registro.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VoiceMigration {
    /// O passo do Gemini virou a conexão da voz.
    pub moved: bool,
    /// Passos da ordem antiga que saíram (os da OpenAI, os de conexão que sumiu e os do Gemini
    /// depois do primeiro).
    pub dropped: usize,
    /// A conexão antiga da transcrição era outra e saiu (agora é uma conexão só).
    pub transcribe_dropped: bool,
}

/// Voz do Nanty (spec 139 T07, G6): só o Gemini, e o texto quando ele falha.
///
/// - **Uma conexão do Google Gemini** (`connection_id`) fala e transcreve. Vazia = automática: a
///   primeira conexão do Google Gemini com chave.
/// - **Modelos e voz** (`speech_model`, `voice`, `transcribe_model`): os iniciais provisórios
///   ficam aqui e só vão para o arquivo quando a pessoa muda.
/// - **Forma antiga** (`order` e `transcribe`, até a rc.22): só lida, nunca gravada. Ao carregar
///   ([`super::Config::load_from`]), [`Self::migrate`] passa o primeiro passo do Gemini para o
///   campo novo e tira os da OpenAI; o Nanty registra o que saiu ao subir.
#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NantyVoice {
    pub enabled: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub connection_id: String,
    #[serde(skip_serializing_if = "is_default_speech_model")]
    pub speech_model: String,
    #[serde(skip_serializing_if = "is_default_voice")]
    pub voice: String,
    #[serde(skip_serializing_if = "is_default_transcribe_model")]
    pub transcribe_model: String,
    #[serde(skip_serializing_if = "is_default_max_input")]
    pub max_input_secs: u64,
    /// A ordem antiga (lida, nunca gravada).
    #[serde(skip_serializing)]
    pub order: Vec<VoiceRoute>,
    /// A transcrição antiga (lida, nunca gravada).
    #[serde(skip_serializing)]
    pub transcribe: NantyRoute,
    /// O resultado da migração ao carregar, até o Nanty registrar (fora do arquivo e da comparação).
    #[serde(skip)]
    pub migrated: Option<VoiceMigration>,
}

impl Default for NantyVoice {
    fn default() -> Self {
        Self {
            enabled: false,
            connection_id: String::new(),
            speech_model: DEFAULT_GEMINI_TTS_MODEL.to_string(),
            voice: DEFAULT_GEMINI_VOICE.to_string(),
            transcribe_model: DEFAULT_TRANSCRIBE_MODEL.to_string(),
            max_input_secs: DEFAULT_VOICE_MAX_INPUT_SECS,
            order: Vec::new(),
            transcribe: NantyRoute::default(),
            migrated: None,
        }
    }
}

/// A comparação ignora o resultado da migração (é só um recado para o registro).
impl PartialEq for NantyVoice {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.connection_id == other.connection_id
            && self.speech_model == other.speech_model
            && self.voice == other.voice
            && self.transcribe_model == other.transcribe_model
            && self.max_input_secs == other.max_input_secs
            && self.order == other.order
            && self.transcribe == other.transcribe
    }
}

fn is_default_speech_model(v: &str) -> bool {
    v == DEFAULT_GEMINI_TTS_MODEL
}

fn is_default_voice(v: &str) -> bool {
    v == DEFAULT_GEMINI_VOICE
}

fn is_default_transcribe_model(v: &str) -> bool {
    v == DEFAULT_TRANSCRIBE_MODEL
}

fn is_default_max_input(v: &u64) -> bool {
    *v == DEFAULT_VOICE_MAX_INPUT_SECS
}

/// O valor escolhido, ou o inicial quando vazio.
fn or_default(value: &str, default: &str) -> String {
    let value = value.trim();
    if value.is_empty() { default.to_string() } else { value.to_string() }
}

impl NantyVoice {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// O modelo da fala (o inicial quando vazio).
    pub fn speech_model(&self) -> String {
        or_default(&self.speech_model, DEFAULT_GEMINI_TTS_MODEL)
    }

    /// A voz (a inicial quando vazia).
    pub fn voice_name(&self) -> String {
        or_default(&self.voice, DEFAULT_GEMINI_VOICE)
    }

    /// O modelo da transcrição (o inicial quando vazio).
    pub fn transcribe_model(&self) -> String {
        or_default(&self.transcribe_model, DEFAULT_TRANSCRIBE_MODEL)
    }

    /// Migra a forma antiga (`order` e `transcribe`, até a rc.22) para a nova (C-16): o primeiro
    /// passo do Gemini (`is_gemini` pelo id da conexão) vira `connection_id`, com o modelo e a voz
    /// dele; os outros passos saem (os da OpenAI, os de conexão que sumiu e os do Gemini depois do
    /// primeiro); o modelo da transcrição antiga passa para `transcribe_model`, e a conexão dela só
    /// vale se não houver outra. Os campos antigos ficam vazios. `None` sem forma antiga. Um campo
    /// novo já escolhido nunca é trocado.
    pub fn migrate(&mut self, is_gemini: impl Fn(&str) -> bool) -> Option<VoiceMigration> {
        if self.order.is_empty() && self.transcribe.connection_id.trim().is_empty() && self.transcribe.model.trim().is_empty() {
            return None;
        }
        let order = std::mem::take(&mut self.order);
        let transcribe = std::mem::take(&mut self.transcribe);
        let mut out = VoiceMigration::default();
        let first = order.iter().position(|r| is_gemini(r.connection_id.trim()));
        for (i, route) in order.iter().enumerate() {
            if Some(i) != first || !self.connection_id.is_empty() {
                out.dropped += 1;
                continue;
            }
            self.connection_id = route.connection_id.trim().to_string();
            if !route.model.trim().is_empty() && is_default_speech_model(&self.speech_model) {
                self.speech_model = route.model.trim().to_string();
            }
            if !route.voice.trim().is_empty() && is_default_voice(&self.voice) {
                self.voice = route.voice.trim().to_string();
            }
            out.moved = true;
        }
        let old = transcribe.connection_id.trim();
        if !old.is_empty() {
            if self.connection_id.is_empty() && is_gemini(old) {
                self.connection_id = old.to_string();
                out.moved = true;
            } else if self.connection_id != old {
                out.transcribe_dropped = true;
            }
        }
        if !transcribe.model.trim().is_empty() && is_default_transcribe_model(&self.transcribe_model) {
            self.transcribe_model = transcribe.model.trim().to_string();
        }
        Some(out)
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
connection_id = "g"
speech_model = "gemini-3.8-flash-lite-tts"
voice = "Puck"
transcribe_model = "m-ouvir"

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
        assert_eq!(
            (n.voice.connection_id.as_str(), n.voice.speech_model().as_str(), n.voice.voice_name().as_str(), n.voice.transcribe_model().as_str()),
            ("g", "gemini-3.8-flash-lite-tts", "Puck", "m-ouvir")
        );
        assert_eq!((n.telegram.chat_id, n.telegram.offset), (Some(42), 7));
        let routes: Vec<&str> = n.routes().into_iter().map(|(_, m)| m).collect();
        assert_eq!(routes, ["m-nanty", "m-reserva"]);
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

    /// Spec 139 T07: a voz nova só grava o que a pessoa mudou; os iniciais provisórios ficam aqui.
    #[test]
    fn voice_defaults_stay_in_the_code() {
        let mut cfg = Config::default();
        cfg.nanty.voice.enabled = true;
        let text = toml::to_string(&cfg).expect("ser");
        let section = text.split("[nanty.voice]").nth(1).expect("seção").trim();
        assert_eq!(section, "enabled = true", "{text}");
        cfg.nanty.voice.voice = "Puck".into();
        let text = toml::to_string(&cfg).expect("ser");
        assert!(text.contains("voice = \"Puck\"") && !text.contains("speech_model") && !text.contains("transcribe_model"), "{text}");
        // Vazio na mão volta ao inicial.
        let empty = NantyVoice { speech_model: String::new(), voice: " ".into(), transcribe_model: String::new(), ..NantyVoice::default() };
        assert_eq!(
            (empty.speech_model(), empty.voice_name(), empty.transcribe_model()),
            (DEFAULT_GEMINI_TTS_MODEL.to_string(), DEFAULT_GEMINI_VOICE.to_string(), DEFAULT_TRANSCRIBE_MODEL.to_string())
        );
    }

    /// A forma do config da rc.22 (só a forma, sem segredos): a ordem com o Gemini e a OpenAI e a
    /// transcrição no Gemini.
    const RC22: &str = r#"
[[connections]]
id = "gemini-pessoal"
name = "Gemini"
base_url = "https://generativelanguage.googleapis.com"
api_key = "env:NANTS_T07_SEM_CHAVE"
kind = "google_gemini"

[[connections]]
id = "openai-direto"
name = "OpenAI direto"
base_url = "https://api.openai.com/v1"
api_key = "env:NANTS_T07_SEM_CHAVE"

[nanty]
connection_id = "gemini-pessoal"
model = "gemini-3.8-flash"

[nanty.voice]
enabled = true
order = [{ connection_id = "openai-direto", model = "gpt-4o-mini-tts-2025-12-15", voice = "marin" }, { connection_id = "gemini-pessoal", voice = "Puck" }]
transcribe = { connection_id = "gemini-pessoal", model = "gemini-3.8-flash" }
max_input_secs = 120
"#;

    /// Spec 139 T07 (C-16): o primeiro passo do Gemini vira a conexão da voz, os da OpenAI saem com
    /// o recado para o registro e a forma antiga nunca volta ao arquivo.
    #[test]
    fn the_rc22_voice_migrates_to_gemini_only() {
        let dir = std::env::temp_dir().join(format!("nants-t07-migra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config.toml");
        std::fs::write(&path, RC22).expect("grava");
        let cfg = Config::load_from(&path).expect("carrega");
        let v = &cfg.nanty.voice;
        assert!(v.enabled);
        assert_eq!(
            (v.connection_id.as_str(), v.speech_model().as_str(), v.voice_name().as_str(), v.transcribe_model().as_str(), v.max_input_secs),
            ("gemini-pessoal", DEFAULT_GEMINI_TTS_MODEL, "Puck", "gemini-3.8-flash", 120)
        );
        assert!(v.order.is_empty() && v.transcribe == NantyRoute::default(), "a forma antiga saiu da memória");
        assert_eq!(v.migrated, Some(VoiceMigration { moved: true, dropped: 1, transcribe_dropped: false }));
        let text = toml::to_string_pretty(&cfg).expect("ser");
        assert!(!text.contains("order =") && !text.contains("transcribe =") && !text.contains("gpt-4o"), "{text}");
        assert!(text.contains("connection_id = \"gemini-pessoal\"") && text.contains("voice = \"Puck\""), "{text}");
        // Gravada e lida de novo: nada a migrar, a mesma voz.
        cfg.save_to(&path).expect("salva");
        let again = Config::load_from(&path).expect("relê");
        assert_eq!(again.nanty.voice, cfg.nanty.voice);
        assert_eq!(again.nanty.voice.migrated, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Só a OpenAI na ordem: a voz fica na automática; a transcrição antiga dá a conexão. Um campo
    /// novo já escolhido nunca é trocado.
    #[test]
    fn migration_without_a_gemini_step_and_with_the_new_field_set() {
        let gemini = |id: &str| id == "g" || id == "g2";
        let mut v = NantyVoice {
            order: vec![VoiceRoute { connection_id: "o".into(), ..VoiceRoute::default() }],
            transcribe: NantyRoute { connection_id: "g".into(), model: String::new() },
            ..NantyVoice::default()
        };
        assert_eq!(v.migrate(gemini), Some(VoiceMigration { moved: true, dropped: 1, transcribe_dropped: false }));
        assert_eq!((v.connection_id.as_str(), v.transcribe_model()), ("g", DEFAULT_TRANSCRIBE_MODEL.to_string()));
        let mut v = NantyVoice {
            connection_id: "g2".into(),
            order: vec![VoiceRoute { connection_id: "g".into(), model: "m".into(), ..VoiceRoute::default() }],
            transcribe: NantyRoute { connection_id: "g".into(), model: String::new() },
            ..NantyVoice::default()
        };
        assert_eq!(v.migrate(gemini), Some(VoiceMigration { moved: false, dropped: 1, transcribe_dropped: true }));
        assert_eq!((v.connection_id.as_str(), v.speech_model()), ("g2", DEFAULT_GEMINI_TTS_MODEL.to_string()));
        assert_eq!(NantyVoice::default().migrate(gemini), None, "sem forma antiga, nada a fazer");
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
