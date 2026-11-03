//! Conversa do Nanty (spec 138 T08, G3, G4 e G7): os comandos fixos, o texto livre com confirmação e
//! a leitura a pedido do projeto citado.
//!
//! - **Uma conversa só:** o que a pessoa escreve no Telegram e na janela entra na mesma conversa, com
//!   o canal de cada mensagem. A resposta sai pelo canal onde ela escreveu (`reply_to`): o que veio do
//!   Telegram é respondido no Telegram; o que veio da janela fica na janela e não vai ao Telegram. As
//!   falas proativas seguem a T07.
//! - **O texto da pessoa é redigido antes de ser gravado:** o [`SecretSet`] troca qualquer segredo
//!   conhecido (as chaves das conexões e o token do bot) por `***`, e só então a mensagem entra na
//!   conversa e no modelo.
//! - **Comandos fixos ([`COMMANDS`]):** só letras latinas minúsculas, números e `_`, nos dois
//!   idiomas, sempre aceitos os dois conjuntos: status, resumo, pausar (vira cartão de confirmação) e
//!   silêncio, mais a ajuda. O Telegram recebe a lista por idioma (`setMyCommands`). Spec 139 T06:
//!   com modelo e orçamento, o status e o resumo são escritos pelo modelo, a partir dos mesmos fatos
//!   (o turno com a entrada estruturada); sem ele, a reserva do dicionário, uma frase por fato e sem
//!   zeros.
//! - **Texto livre (spec 139 T03, C-01):** com modelo, vai sempre ao laço de agente
//!   ([`super::agent`]), sem casamento por nome antes: o modelo resolve o projeto pela intenção e
//!   consulta o que precisa pelas ferramentas de leitura ([`super::tools`]). A resposta final é texto
//!   puro e passa pelo `clean` da T07 antes de ser gravada. A quem escreveu pelo Telegram, o
//!   "digitando" fica ligado enquanto o laço roda.
//! - **Ações e silêncio pela conversa (spec 139 T04, G3):** o pedido de ação em frase livre vira o
//!   cartão de confirmação pela ferramenta `propose_action` (sempre, mesmo para a ação automática: só
//!   o clique executa), e a mensagem cita o cartão. O silêncio pedido em frase livre vale direto, com
//!   a confirmação do servidor e o botão para desligar ([`super::silence`]); num turno contaminado,
//!   vira cartão.
//! - **Abrir o Nants (spec 140 T03, C-30):** o pedido vira a ferramenta `open_nants`, e o servidor
//!   acrescenta o caminho ([`super::link`]), como a confirmação do silêncio: o modelo nunca escreve o
//!   endereço.
//! - **Procedência (spec 140 T04, C-01):** a resposta grava de onde veio, com a contaminação da sessão
//!   e todos os projetos consultados ([`super::provenance`]).
//! - **Texto de terceiro (spec 139 T04, C-07):** a mensagem encaminhada (ou com a citação de outra
//!   pessoa) vai ao modelo como dado não confiável e contamina o turno; a encaminhada nunca vale como
//!   comando.
//! - **Reserva sem modelo** (sem modelo, sem orçamento, fila cheia ou falha): o [`cite`] acha o
//!   projeto citado, com normalização (sem acento; "14" casa com "Test 14"), e o Nanty diz a fase dele
//!   pela fonte única; citação ambígua ou desconhecida vira a lista para escolher, com os candidatos
//!   prováveis primeiro. O `cite` também serve ao atalho `/pausar <nome>`.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use super::act::{self, NoCard, Requestable};
use super::actions::ActionId;
use super::agent::{self, Ended, Session, SilenceEffect};
use super::can_do::CanDo;
use super::journal::{JournalEntry, JournalKind};
use super::silence::Request;
use super::store::{AudioRef, Channel, ConversationMessage, Relayed, Speaker};
use super::telegram::format;
use super::turn::{TurnInput, TurnKind, TurnOutcome};
use super::voice::Tone;
use super::{digest, nanty_lang, tools, turn, Nanty};
use crate::config::{Config, DigestEmpty, NantyConfig, Project, Urgency};
use crate::i18n::{tr, Lang};
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Maior mensagem da pessoa guardada (o limite do Telegram).
pub const MAX_PERSON_CHARS: usize = 4096;
/// Maior citação de terceiro guardada (spec 139 T05, vindo da revisão da T04): o trecho do
/// `quote` do Telegram vai até 1024 caracteres; a resposta a uma mensagem de terceiro sem trecho
/// traz o texto dela inteiro, que fica neste corte.
pub const MAX_QUOTE_CHARS: usize = 1024;
/// Silêncio do comando sem número, em horas.
pub const SILENCE_DEFAULT_HOURS: u64 = 1;
/// Maior silêncio pelo comando, em horas.
pub const SILENCE_MAX_HOURS: u64 = 24;
/// Projetos listados quando o Nanty pede para escolher (os que casam com a citação nunca são
/// cortados).
const MAX_LISTED_PROJECTS: usize = 10;

/// Um comando fixo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Status,
    Summary,
    Pause,
    Silence,
    Help,
}

impl Command {
    /// Código da descrição do comando no dicionário (a lista do Telegram).
    fn about_code(self) -> &'static str {
        match self {
            Command::Status => "nanty.command.status",
            Command::Summary => "nanty.command.summary",
            Command::Pause => "nanty.command.pause",
            Command::Silence => "nanty.command.silence",
            Command::Help => "nanty.command.help",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Command::Status => "status",
            Command::Summary => "summary",
            Command::Pause => "pause",
            Command::Silence => "silence",
            Command::Help => "help",
        }
    }
}

/// Os dois conjuntos de comandos, `(nome, idioma do conjunto, comando)`. Os dois valem sempre; o
/// idioma só decide qual lista o Telegram mostra.
pub const COMMANDS: [(&str, Lang, Command); 10] = [
    ("status", Lang::PtBr, Command::Status),
    ("resumo", Lang::PtBr, Command::Summary),
    ("pausar", Lang::PtBr, Command::Pause),
    ("silencio", Lang::PtBr, Command::Silence),
    ("ajuda", Lang::PtBr, Command::Help),
    ("status", Lang::En, Command::Status),
    ("summary", Lang::En, Command::Summary),
    ("pause", Lang::En, Command::Pause),
    ("quiet", Lang::En, Command::Silence),
    ("help", Lang::En, Command::Help),
];

/// Nome de comando aceito pelo Telegram: letras latinas minúsculas, números e `_`, até 32.
pub fn valid_command_name(name: &str) -> bool {
    (1..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A lista de comandos de um idioma, com a descrição do dicionário (o `setMyCommands`).
pub fn command_menu(lang: Lang) -> Vec<(&'static str, String)> {
    COMMANDS.iter().filter(|(_, l, _)| *l == lang).map(|(name, _, c)| (*name, tr(lang, c.about_code(), &[]))).collect()
}

/// Os nomes dos comandos de um idioma, para o texto da ajuda (e para o `render`, que monta a
/// lista no idioma do texto na hora do envio).
pub(super) fn command_names(lang: Lang) -> String {
    COMMANDS.iter().filter(|(_, l, _)| *l == lang).map(|(name, _, _)| *name).collect::<Vec<_>>().join(", ")
}

/// O que a pessoa escreveu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parsed<'a> {
    /// Um comando conhecido e o resto da linha.
    Command(Command, &'a str),
    /// Começa com `/`, mas não é comando conhecido.
    Unknown,
    /// Texto livre.
    Text,
}

/// Lê o comando: `/nome`, `/nome@bot` e o resto como argumento, sem diferença de maiúsculas. O
/// `/start` do dono já pareado mostra a ajuda.
pub fn parse(text: &str) -> Parsed<'_> {
    let Some(rest) = text.trim().strip_prefix('/') else { return Parsed::Text };
    let (head, args) = match rest.split_once(char::is_whitespace) {
        Some((head, args)) => (head, args.trim()),
        None => (rest, ""),
    };
    let name = head.split('@').next().unwrap_or_default().to_ascii_lowercase();
    if name == "start" {
        return Parsed::Command(Command::Help, "");
    }
    COMMANDS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map_or(Parsed::Unknown, |(_, _, command)| Parsed::Command(*command, args))
}

// ---- A mensagem da pessoa ----

/// O texto da pessoa como fica gravado: os segredos conhecidos viram `***` (antes do corte, para
/// nunca sobrar um pedaço) e o corte fica no limite. `None` sem texto.
pub fn redact(secrets: &SecretSet, raw: &str) -> Option<String> {
    redact_to(secrets, raw, MAX_PERSON_CHARS)
}

/// Como [`redact`], com o corte em `max` caracteres.
fn redact_to(secrets: &SecretSet, raw: &str, max: usize) -> Option<String> {
    let scrubbed = secrets.scrub_exact(raw.trim());
    let text: String = scrubbed.trim().chars().take(max).collect();
    (!text.trim().is_empty()).then_some(text)
}

/// Grava a mensagem da pessoa na conversa (redigida) e a devolve. `None` sem texto.
pub fn person_message(nanty: &Nanty, cfg: &NantyConfig, secrets: &SecretSet, channel: Channel, raw: &str) -> Option<ConversationMessage> {
    person_message_with(nanty, cfg, secrets, channel, Some(raw), false, None)
}

/// Como [`person_message`], com a marca da nota de voz (T09): o texto é a transcrição (redigida do
/// mesmo jeito) e o áudio é a nota guardada. A nota que não virou texto entra sem texto; sem a
/// marca, `None` sem texto.
pub fn person_message_with(
    nanty: &Nanty,
    cfg: &NantyConfig,
    secrets: &SecretSet,
    channel: Channel,
    raw: Option<&str>,
    voice: bool,
    audio: Option<AudioRef>,
) -> Option<ConversationMessage> {
    person_message_from(nanty, cfg, secrets, channel, raw, voice, audio, None)
}

/// Como [`person_message_with`], com o texto de terceiro que a mensagem traz (spec 139 T04): a
/// citação também é redigida e cortada em [`MAX_QUOTE_CHARS`] antes de ser gravada. O idioma da
/// mensagem (spec 139 T05) é resolvido aqui ([`super::language::person_lang`]) e gravado em `lang`;
/// quando vem de um sinal dela (o texto ou o `language_code`), vira o último idioma da pessoa.
#[allow(clippy::too_many_arguments)]
pub fn person_message_from(
    nanty: &Nanty,
    cfg: &NantyConfig,
    secrets: &SecretSet,
    channel: Channel,
    raw: Option<&str>,
    voice: bool,
    audio: Option<AudioRef>,
    relayed: Option<Relayed>,
) -> Option<ConversationMessage> {
    let relayed = relayed.map(|r| Relayed { quote: r.quote.as_deref().and_then(|q| redact_to(secrets, q, MAX_QUOTE_CHARS)), ..r });
    let text = raw.and_then(|r| redact(secrets, r));
    if text.is_none() && !voice {
        return None;
    }
    // O texto encaminhado é de terceiro: não diz o idioma de quem encaminhou.
    let own = text.as_deref().filter(|_| !relayed.as_ref().is_some_and(|r| r.forwarded));
    let lang = nanty.note_person_lang(cfg, channel, own);
    let message = ConversationMessage {
        id: super::new_id(),
        at_ms: nanty.now().epoch_ms,
        from: Speaker::Person,
        channel,
        project_id: None,
        text,
        code: None,
        params: Map::new(),
        lang: Some(lang.tag().to_string()),
        card_ids: Vec::new(),
        actions: Vec::new(),
        reply_to: None,
        voice,
        audio,
        relayed,
        project_ids: Vec::new(),
        // A procedência da pessoa é o `relayed`.
        taint: None,
        seq: None,
    };
    nanty.push_message(&message);
    Some(message)
}

/// Responde numa tarefa à parte (a escuta do Telegram e a rota da janela não esperam o modelo).
pub fn spawn_reply(state: &AppState, person: ConversationMessage) {
    let state = state.clone();
    tokio::spawn(async move {
        reply(&state, &person).await;
    });
}

/// A resposta à mensagem da pessoa: o comando (o status e o resumo pelo modelo, quando há; o resto fixo) ou o texto livre (com o modelo). Sai pelo
/// canal da mensagem. Devolve a resposta gravada.
pub async fn reply(state: &AppState, person: &ConversationMessage) -> Option<ConversationMessage> {
    let nanty = Arc::clone(state.nanty());
    let text = person.text.clone()?;
    let cfg = state.config().read().await.clone();
    // Spec 139 T05: a resposta sai no idioma de quem falou, o resolvido na mensagem dela.
    let lang = super::language::tagged(person.lang.as_deref()).unwrap_or_else(|| nanty_lang(&cfg.nanty, &nanty));
    let channel = person.channel;
    // A mensagem encaminhada é de terceiro: nunca vale como comando (C-07).
    let forwarded = person.relayed.as_ref().is_some_and(|r| r.forwarded);
    let parsed = if forwarded { Parsed::Text } else { parse(&text) };
    // Spec 140 T01: só o texto do modelo tem tom; a resposta fixa fala neutro.
    let (kind, mut message, tone) = match parsed {
        Parsed::Command(command, args) => (command.as_str(), command_reply(state, &nanty, &cfg, lang, command, args, channel).await, Tone::Neutral),
        Parsed::Unknown => ("unknown", fixed(&nanty, "nanty.chat.unknown_command", with_commands(lang)), Tone::Neutral),
        Parsed::Text => {
            let (message, tone) = converse(state, &nanty, &cfg, lang, person, &text).await;
            ("text", message, tone)
        }
    };
    message.channel = channel;
    message.reply_to = Some(person.id.clone());
    // Toda mensagem leva o idioma (T05): o `render` e a janela montam o texto fixo nele.
    if message.lang.is_none() {
        message.lang = Some(lang.tag().to_string());
    }
    let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, "chat.reply");
    entry.project_id = message.project_id.clone();
    entry.params.insert("input".into(), json!(kind));
    entry.params.insert("channel".into(), json!(channel));
    entry.params.insert("cards".into(), json!(message.card_ids.len()));
    if let Some(code) = &message.code {
        entry.params.insert("reply".into(), json!(code));
    }
    nanty.record(&entry);
    // Spec 139 T08: o texto entra na conversa agora; com a voz ligada, a fala vem depois, e quem
    // escreveu pelo Telegram vê o "gravando áudio" enquanto ela é gerada.
    super::voice::deliver(state, &nanty, &mut message, tone, channel == Channel::Telegram).await;
    // Spec 140 T06: o degrau da janela roda depois da resposta, já entregue, fora da fila dos turnos.
    super::summarize::after_reply(state, &nanty).await;
    Some(message)
}

/// Mensagem do Nanty com o texto fixo do dicionário.
fn fixed(nanty: &Nanty, code: &str, params: Map<String, Value>) -> ConversationMessage {
    ConversationMessage::from_nanty(super::new_id(), nanty.now().epoch_ms, Channel::Web, code, params)
}

fn param(name: &str, value: impl Into<Value>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(name.to_string(), value.into());
    map
}

fn with_commands(lang: Lang) -> Map<String, Value> {
    param("commands", command_names(lang))
}

/// Hora local `HH:MM` de um instante.
fn hhmm(at_ms: u64) -> String {
    super::clock::local_time_at(at_ms).map(|t| format!("{:02}:{:02}", t.hour, t.minute)).unwrap_or_default()
}

// ---- Comandos ----

async fn command_reply(
    state: &AppState,
    nanty: &Arc<Nanty>,
    cfg: &Config,
    lang: Lang,
    command: Command,
    args: &str,
    channel: Channel,
) -> ConversationMessage {
    match command {
        Command::Status => status(state, nanty, cfg, lang, channel).await,
        Command::Summary => summary(state, nanty, cfg, lang, channel).await,
        Command::Pause => pause(state, nanty, cfg, args, channel).await,
        Command::Silence => silence(nanty, args, channel),
        // Spec 139 T13: a conversa livre depende do modelo; sem ele, a ajuda não a promete.
        Command::Help => {
            let code = if turn::model_route(cfg).is_some() { "nanty.chat.help" } else { "nanty.chat.help_no_model" };
            fixed(nanty, code, with_commands(lang))
        }
    }
}

/// O atalho escrito pelo modelo (spec 139 T06, G4): com modelo e orçamento, o turno com a entrada
/// estruturada, no idioma de quem falou. `None` sem modelo, sem orçamento, com a fila cheia, com
/// falha ou com a resposta inválida: fica a reserva do dicionário.
async fn written(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, input: &TurnInput, lang: Lang) -> Option<ConversationMessage> {
    turn::model_route(cfg)?;
    match turn::run_in(state, nanty, input, lang).await {
        TurnOutcome::Reply(reply) => Some(turn::model_message(nanty, lang, &reply, Vec::new())),
        TurnOutcome::Skipped(_) => None,
    }
}

/// Uma linha extra, `{code, params}`, como no resumo da manhã.
fn line(code: &str, params: Value) -> Value {
    json!({ "code": code, "params": params })
}

/// `status`: o que espera a pessoa, o que foi feito hoje, o previsto, o silêncio e o orçamento. Com
/// modelo, o texto é dele, a partir desses fatos (spec 139 T06); sem ele, uma frase por fato, sem
/// zeros.
async fn status(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, lang: Lang, channel: Channel) -> ConversationMessage {
    let now = nanty.now();
    let snap = nanty.snapshot();
    let waiting = snap
        .cards
        .iter()
        .filter(|c| c.expires_at_ms > now.epoch_ms && c.urgency != Some(Urgency::Digest) && c.awaits_person())
        .count() as u64;
    let start = digest::day_start_ms(&now);
    let done = snap.done.iter().filter(|d| d.at_ms >= start).count() as u64;
    // O resumo da manhã está sempre agendado: o previsto aqui é o que vem além dele.
    let planned = snap.planned.len() as u64;
    let silence = tools::global_silence(nanty, now.epoch_ms);

    let mut input = TurnInput::new(TurnKind::Status);
    for (name, value) in [("waiting", waiting), ("done_today", done), ("planned_later", planned)] {
        if value > 0 {
            input.counts.insert(name, value);
        }
    }
    input.signals = snap
        .alerts
        .values()
        .filter(|a| a.project_id.as_deref().is_none_or(|p| cfg.nanty.observes(p)))
        .filter_map(turn::SignalFact::of_alert)
        .take(turn::MAX_SIGNALS_IN)
        .collect();
    input.silenced_until = silence.map(hhmm);
    let view = turn::budget_view(cfg, &snap.spend_on(&now.day_key()));
    match view.mode {
        turn::BudgetMode::Usd => {
            if view.spent_usd > 0.0 {
                input.cost_usd.insert("nanty_today", (view.spent_usd * 10_000.0).round() / 10_000.0);
            }
            input.cost_usd.insert("nanty_daily_budget", view.budget_usd);
        }
        turn::BudgetMode::Tokens => {
            if view.tokens > 0 {
                input.tokens.insert("nanty_today", view.tokens);
            }
            input.tokens.insert("nanty_daily_budget", view.budget_tokens);
        }
    }
    input.can_do = CanDo::now(state, cfg, nanty, channel);
    let projects = input.project_ids();
    if let Some(mut message) = written(state, nanty, cfg, &input, lang).await {
        message.link_projects(&projects);
        return message;
    }

    let mut lines = Vec::new();
    if waiting > 0 {
        lines.push(line("nanty.chat.status_waiting", json!({ "count": waiting })));
    } else {
        lines.push(line("nanty.chat.status_nothing_waiting", json!({})));
    }
    if done > 0 {
        lines.push(line("nanty.chat.status_done", json!({ "count": done })));
    }
    if planned > 0 {
        lines.push(line("nanty.chat.status_planned", json!({ "count": planned })));
    }
    if let Some(until) = silence {
        lines.push(line("nanty.chat.status_silence", json!({ "until": hhmm(until) })));
    }
    let (code, budget) = tools::budget_line(nanty, cfg);
    lines.push(line(&code, Value::Object(budget)));
    let mut message = fixed(nanty, "nanty.chat.status", param("lines", Value::Array(lines)));
    message.link_projects(&projects);
    message
}

/// `resumo`: os fatos do resumo da manhã, agora, sem consumir os avisos guardados. Com modelo, o
/// texto é dele (spec 139 T06); sem ele, a reserva do dicionário.
async fn summary(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, lang: Lang, channel: Channel) -> ConversationMessage {
    let now = nanty.now();
    let facts = digest::gather(state, nanty, &now).await;
    let mut input = digest::summary_input(TurnKind::Summary, &facts, &nanty.snapshot());
    input.can_do = CanDo::now(state, cfg, nanty, channel);
    let projects = digest::linked_projects(&facts, &input);
    if let Some(mut message) = written(state, nanty, cfg, &input, lang).await {
        message.link_projects(&projects);
        return message;
    }
    let mut short = cfg.nanty.clone();
    short.digest_empty = DigestEmpty::Short;
    let mut message = match digest::compose(&facts, &short, super::new_id(), now.epoch_ms) {
        Some(mut message) => {
            let code = if message.code.as_deref() == Some(digest::QUIET) { "nanty.chat.summary_quiet" } else { "nanty.chat.summary" };
            message.code = Some(code.to_string());
            message
        }
        None => fixed(nanty, "nanty.chat.summary_quiet", param("projects", facts.projects)),
    };
    message.link_projects(&projects);
    message
}

/// Os projetos que o Nanty observa (os únicos que a conversa lê).
pub(super) fn observed(cfg: &Config) -> Vec<&Project> {
    cfg.projects.iter().filter(|p| cfg.nanty.observes(&p.id)).collect()
}

/// "Escolha um projeto": os nomes na ordem de [`ranked`] (os prováveis primeiro), até
/// [`MAX_LISTED_PROJECTS`], sem nunca cortar um projeto que casa com a citação.
fn choose(nanty: &Nanty, projects: &[&Project], text: &str) -> ConversationMessage {
    if projects.is_empty() {
        return fixed(nanty, "nanty.chat.no_projects", Map::new());
    }
    let ranked = ranked(projects, text);
    let matching = ranked.iter().filter(|(_, score)| *score > 0).count();
    let names: Vec<&str> = ranked.iter().take(MAX_LISTED_PROJECTS.max(matching)).map(|(p, _)| p.name.as_str()).collect();
    fixed(nanty, "nanty.chat.choose_project", param("projects", names.join(", ")))
}

/// `pausar [projeto]`: o cartão de confirmação para pausar a execução em andamento. Sem nome, vale a
/// única execução em andamento; com várias, o Nanty pede para escolher.
async fn pause(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, args: &str, channel: Channel) -> ConversationMessage {
    let projects = observed(cfg);
    let running: Vec<&Project> = projects.iter().copied().filter(|p| state.runs().active(&p.id).is_some()).collect();
    let target = if args.is_empty() {
        match running.as_slice() {
            [] => return fixed(nanty, "nanty.chat.pause_nothing", Map::new()),
            [one] => *one,
            many => return choose(nanty, many, args),
        }
    } else {
        match cite(&projects, args) {
            Cited::One(p) => p,
            Cited::Ambiguous(list) => return choose(nanty, &list, args),
            Cited::None | Cited::Unknown => return choose(nanty, &projects, args),
        }
    };
    let name = param("project", target.name.as_str());
    let req = Requestable { action: ActionId::PauseRun, group: act::pause_group(&target.id), project_id: Some(target.id.clone()) };
    let mut message = match act::request_card(state, &req, channel).await {
        Ok(card) => {
            let mut m = fixed(nanty, "nanty.chat.pause_card", name);
            m.card_ids = vec![card];
            m.actions = vec![ActionId::PauseRun.as_str().to_string()];
            m
        }
        Err(NoCard::Off) => fixed(nanty, "nanty.chat.action_off", Map::new()),
        Err(NoCard::Gone) => fixed(nanty, "nanty.chat.pause_not_running", name),
        Err(NoCard::Unavailable) => fixed(nanty, "nanty.outcome.unavailable", Map::new()),
    };
    message.project_id = Some(target.id.clone());
    message
}

/// `silencio [horas]`: silêncio de todos os avisos por 1 hora (ou pelas horas pedidas, até
/// [`SILENCE_MAX_HOURS`]); `0` (ou `off`, `desligar`) desliga. Ligado, a resposta cita o cartão com o
/// botão para desligar (spec 139 T04).
fn silence(nanty: &Nanty, args: &str, channel: Channel) -> ConversationMessage {
    let arg = args.trim().to_lowercase();
    let hours = match arg.as_str() {
        "" => Some(SILENCE_DEFAULT_HOURS),
        "off" | "desligar" => Some(0),
        other => other.parse::<u64>().ok(),
    };
    let Some(hours) = hours.filter(|h| *h <= SILENCE_MAX_HOURS) else {
        return fixed(nanty, "nanty.chat.silence_usage", param("max", SILENCE_MAX_HOURS));
    };
    if hours == 0 {
        super::silence::end(nanty, channel);
        return fixed(nanty, "nanty.chat.silence_off", Map::new());
    }
    let (until, card) = super::silence::apply(nanty, Request::Hours(hours), channel);
    let mut message = fixed(nanty, "nanty.chat.silence_on", param("until", hhmm(until)));
    message.card_ids = vec![card];
    message
}

// ---- O projeto citado ----

/// O projeto que o texto cita.
#[derive(Debug, Clone, PartialEq)]
pub enum Cited<'a> {
    /// Nenhum projeto citado.
    None,
    One(&'a Project),
    /// O texto fala de um projeto ("projeto X") que não existe entre os observados.
    Unknown,
    /// Mais de um projeto citado.
    Ambiguous(Vec<&'a Project>),
}

/// Letra sem acento (a citação "sao joao" casa com o nome "São João").
fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        other => other,
    }
}

/// A palavra sem acento (para a detecção do idioma, [`super::language`]).
pub(super) fn fold_word(word: &str) -> String {
    word.chars().map(fold).collect()
}

/// O texto em palavras minúsculas e sem acento, cercado de espaços: `" olha o projeto loja "`.
fn words(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_alphanumeric() { fold(c.to_lowercase().next().unwrap_or(c)) } else { ' ' })
        .collect();
    format!(" {} ", flat.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Palavras depois de "projeto" que não são nome (`o projeto que falhou`), já sem acento.
const NOT_A_NAME: [&str; 23] = [
    "que", "de", "do", "da", "dos", "das", "no", "na", "em", "e", "ou", "esta", "agora", "pra", "para", "that", "is",
    "the", "of", "in", "on", "and", "now",
];

/// Como o texto cita um projeto: pelo nome inteiro (ou pelo id), por um número do nome ("14" em
/// "Test 14") ou por uma palavra do nome (esta só ordena a lista de escolha).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Hit {
    whole: bool,
    number: bool,
    word: bool,
}

impl Hit {
    fn score(self) -> u8 {
        if self.whole {
            3
        } else if self.number {
            2
        } else {
            u8::from(self.word)
        }
    }
}

fn hit(hay: &str, project: &Project) -> Hit {
    let name = words(&project.name);
    let id = words(&project.id);
    let whole = [name.as_str(), id.as_str()].iter().any(|w| w.trim().len() > 1 && hay.contains(*w));
    let number = name
        .split_whitespace()
        .filter(|w| w.bytes().all(|b| b.is_ascii_digit()))
        .any(|n| hay.contains(&format!(" {n} ")));
    let word = name
        .split_whitespace()
        .filter(|w| w.chars().count() >= 3 && !NOT_A_NAME.contains(w) && !matches!(*w, "projeto" | "project"))
        .any(|w| hay.contains(&format!(" {w} ")));
    Hit { whole, number, word }
}

/// Os projetos na ordem da lista de escolha: os que casam com a citação primeiro (nome inteiro,
/// depois número, depois uma palavra do nome), e o resto na ordem de antes. Com a nota de cada um.
fn ranked<'a>(projects: &[&'a Project], text: &str) -> Vec<(&'a Project, u8)> {
    let hay = words(text);
    let mut out: Vec<(&'a Project, u8)> = projects.iter().map(|p| (*p, hit(&hay, p).score())).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

/// O projeto citado: pelo nome (ou pelo id), como palavras inteiras e sem diferença de acento; sem
/// nome inteiro no texto, por um número do nome ("o projeto 14" casa com "Test 14"). Um nome contido
/// em outro citado junto (`Loja` dentro de `Loja Nova`) não conta à parte.
pub fn cite<'a>(projects: &[&'a Project], text: &str) -> Cited<'a> {
    let hay = words(text);
    let hits: Vec<(&'a Project, Hit)> = projects.iter().map(|p| (*p, hit(&hay, p))).collect();
    let any_whole = hits.iter().any(|(_, h)| h.whole);
    let mut found: Vec<(&'a Project, String)> = Vec::new();
    for (p, h) in &hits {
        let counts = if any_whole { h.whole } else { h.number };
        if counts && !found.iter().any(|(f, _)| f.id == p.id) {
            found.push((*p, words(&p.name)));
        }
    }
    if any_whole {
        let names: Vec<String> = found.iter().map(|(_, n)| n.clone()).collect();
        found.retain(|(_, name)| !names.iter().any(|other| other != name && other.contains(name.as_str())));
    }
    match found.len() {
        0 => {
            let tokens: Vec<&str> = hay.split_whitespace().collect();
            let named = tokens.windows(2).any(|w| matches!(w[0], "projeto" | "project") && !NOT_A_NAME.contains(&w[1]));
            if named {
                Cited::Unknown
            } else {
                Cited::None
            }
        }
        1 => Cited::One(found[0].0),
        _ => Cited::Ambiguous(found.into_iter().map(|(p, _)| p).collect()),
    }
}

// ---- Texto livre ----

/// O texto livre (spec 139 T03): com modelo, sempre o laço de agente, com o "digitando" para quem
/// escreveu pelo Telegram; o texto final passa pelo `clean`. Sem modelo, sem orçamento, com a fila
/// cheia ou com falha, a reserva ([`reserve`]). O que as ferramentas fizeram (T04) vale em qualquer
/// saída: a mensagem cita os cartões, e a confirmação do silêncio é do servidor. Devolve também o tom
/// da fala (spec 140 T01): o que o laço leu, só quando sobrou texto do modelo.
async fn converse(
    state: &AppState,
    nanty: &Arc<Nanty>,
    cfg: &Config,
    lang: Lang,
    person: &ConversationMessage,
    text: &str,
) -> (ConversationMessage, Tone) {
    if turn::model_route(cfg).is_none() {
        return (reserve(state, nanty, cfg, lang, text, "nanty.chat.no_model").await, Tone::Neutral);
    }
    let input = agent::input(state, nanty, cfg, lang, person, text).await;
    let mut session = Session::new(person.channel, input.taint, lang);
    session.person_id = Some(person.id.clone());
    // O texto encaminhado é de terceiro: não conta como o que ela disse agora.
    let forwarded = person.relayed.as_ref().is_some_and(|r| r.forwarded);
    session.person_text = (!forwarded).then(|| text.to_string());
    let (mut message, tone) = answer(state, nanty, cfg, lang, person, text, input.messages, &mut session).await;
    // Spec 140 T04 (C-01 e C-16): a procedência fica gravada em toda saída, mesmo na reserva, e os
    // projetos consultados ficam todos na mensagem, não só quando há um.
    message.taint = Some(session.tainted.clone());
    message.link_projects(&session.read);
    (message, tone)
}

/// O laço e a resposta montada a partir do que ele deixou na `session`.
#[allow(clippy::too_many_arguments)]
async fn answer(
    state: &AppState,
    nanty: &Arc<Nanty>,
    cfg: &Config,
    lang: Lang,
    person: &ConversationMessage,
    text: &str,
    context: Vec<crate::agents::llm_client::ChatMessage>,
    session: &mut Session,
) -> (ConversationMessage, Tone) {
    let system = crate::agents::prompts::nanty_chat_system(lang);
    // C-09: só a quem escreveu pelo Telegram; a fila de saída o renova sem atropelar os envios.
    let typing = (person.channel == Channel::Telegram).then(|| nanty.telegram().outbox().typing());
    let ended = agent::run(state, nanty, cfg, system, context, session).await;
    drop(typing);
    // Spec 140 T07 (C-24): o que o turno contaminado pediu à memória vira um cartão só, no fim.
    let proposals = std::mem::take(&mut session.memory.proposals);
    if let Some(card) = super::memory::card::flush(nanty, cfg, &proposals, session.channel) {
        session.memory.cards.push(card);
    }
    session.memory.proposals = proposals;
    let secrets = SecretSet::from_config(&*state.config().read().await);
    let mut tone = Tone::Neutral;
    let reply = match ended {
        Ended::Reply { text, tone: chosen, .. } => {
            // Spec 140 T14 (C-27): a limpeza sem o corte. A conversa grava a resposta inteira; quem
            // divide é o Telegram, em partes, e a frase do servidor continua fechando a última.
            let clean = Some(format::clean_whole(&secrets, &text)).filter(|t| !t.is_empty());
            if clean.is_some() {
                tone = chosen;
            }
            clean
        }
        Ended::Skipped(reason) => {
            let code = match reason {
                "budget" => "nanty.chat.budget",
                "busy" => "nanty.chat.busy",
                "no_model" => "nanty.chat.no_model",
                _ => "nanty.chat.failed",
            };
            // Sem texto do modelo e sem nada feito pelas ferramentas: a reserva.
            if session.cards.is_empty() && session.silence.is_none() && session.open.is_none() && session.memory.is_empty() {
                return (reserve(state, nanty, cfg, lang, text, code).await, Tone::Neutral);
            }
            None
        }
    };
    let effect = effect_message(nanty, session);
    let mut message = match (effect, reply) {
        (Some(mut m), reply) => {
            m.text = reply;
            m
        }
        (None, Some(reply)) => {
            let mut m = fixed(nanty, "nanty.chat.failed", Map::new());
            m.code = None;
            m.text = Some(reply);
            m
        }
        (None, None) => return (fixed(nanty, "nanty.chat.failed", with_commands(lang)), Tone::Neutral),
    };
    // O idioma do texto do modelo (instruído a seguir a pessoa); sem sinal claro, o dela.
    let written = message.text.as_deref().and_then(super::language::detect).unwrap_or(lang);
    message.lang = Some(written.tag().to_string());
    message.card_ids = session.cards.iter().chain(&session.memory.cards).cloned().collect();
    message.actions = session.actions.clone();
    (message, tone)
}

/// A parte do servidor na resposta (T04): a confirmação do silêncio, com a hora calculada aqui, ou o
/// lembrete de que nada é feito sem o toque no cartão. Spec 140 T03: o caminho até o Nants entra como
/// a frase da mensagem, ou como mais uma linha quando já há outra. `None` sem nada feito pelas
/// ferramentas.
fn effect_message(nanty: &Nanty, session: &Session) -> Option<ConversationMessage> {
    let base = match session.silence {
        Some(SilenceEffect::Applied(until)) => Some(fixed(nanty, "nanty.chat.silence_set", param("until", hhmm(until)))),
        Some(SilenceEffect::Proposed(until)) => Some(fixed(nanty, "nanty.chat.silence_confirm", param("until", hhmm(until)))),
        Some(SilenceEffect::Ended) => Some(fixed(nanty, "nanty.chat.silence_off", Map::new())),
        None if !session.cards.is_empty() => Some(fixed(nanty, "nanty.chat.confirm", Map::new())),
        None => None,
    };
    // As linhas extras: o caminho até o Nants (spec 140 T03) e o rodapé da memória (spec 140 T07).
    let mut extra: Vec<(String, Map<String, Value>)> = Vec::new();
    if let Some(open) = &session.open {
        extra.push((open.code().to_string(), open.params()));
    }
    for (code, params) in session.memory.lines() {
        extra.push((code.to_string(), params));
    }
    let mut message = match base {
        Some(message) => message,
        None if extra.is_empty() => return None,
        None => {
            let (code, params) = extra.remove(0);
            fixed(nanty, &code, params)
        }
    };
    if !extra.is_empty() {
        let why: Vec<Value> = extra.iter().map(|(code, params)| line(code, Value::Object(params.clone()))).collect();
        message.params.insert(digest::WHY.to_string(), Value::Array(why));
    }
    // Os parâmetros do caminho também ficam na mensagem: a janela e o Telegram os leem dali.
    if let Some(open) = &session.open {
        message.params.extend(open.params());
    }
    Some(message)
}

/// A reserva sem modelo: o projeto citado ([`cite`]) e a fase dele pela fonte única, nas linhas que a
/// janela também traduz; citação ambígua ou desconhecida, a lista para escolher, com os prováveis
/// primeiro; sem citação, o texto fixo `code`.
async fn reserve(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, lang: Lang, text: &str, code: &str) -> ConversationMessage {
    let projects = observed(cfg);
    match cite(&projects, text) {
        Cited::None => fixed(nanty, code, with_commands(lang)),
        Cited::Ambiguous(_) | Cited::Unknown => choose(nanty, &projects, text),
        Cited::One(project) => match super::observe::project_facts(state, cfg, &project.id).await {
            Some(facts) => {
                let lines: Vec<Value> = tools::status_lines(&facts, false)
                    .into_iter()
                    .map(|(code, params)| json!({ "code": code, "params": params }))
                    .collect();
                let mut params = param("project", project.name.as_str());
                params.insert("lines".into(), Value::Array(lines));
                let mut message = fixed(nanty, "nanty.chat.reserve_status", params);
                message.project_id = Some(project.id.clone());
                message
            }
            None => fixed(nanty, code, with_commands(lang)),
        },
    }
}

#[cfg(test)]
#[path = "t08_tests.rs"]
pub(crate) mod tests;
