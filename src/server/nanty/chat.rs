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
//! - **Comandos fixos, sem modelo ([`COMMANDS`]):** só letras latinas minúsculas, números e `_`, nos
//!   dois idiomas, sempre aceitos os dois conjuntos: status, resumo, pausar (vira cartão de
//!   confirmação) e silêncio, mais a ajuda. O Telegram recebe a lista por idioma (`setMyCommands`).
//! - **Texto livre:** vai ao modelo pela fila única dos turnos ([`turn::run_call`]), com o orçamento
//!   do dia. Quando o texto pede uma ação, o modelo só propõe ids do que [`act::requestable`] ofereceu,
//!   e o servidor sempre responde com um cartão de confirmação ([`act::request_card`]), mesmo para a
//!   ação automática. A resposta do modelo passa pelo `clean` da T07 antes de ser gravada.
//! - **Leitura a pedido (C-13, [`super::read`]):** só do projeto citado pelo nome. Projeto que não
//!   existe ou citação ambígua: o Nanty pede para escolher, sem chamar o modelo. A leitura entra no
//!   prompt como dado não confiável.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Map, Value};

use super::act::{self, NoCard, Requestable};
use super::actions::ActionId;
use super::journal::{JournalEntry, JournalKind};
use super::store::{AudioRef, Channel, ConversationMessage, Silence, Speaker};
use super::telegram::format;
use super::turn::{self, Call, SignalFact, TurnKind, TurnOutcome};
use super::{digest, nanty_lang, Nanty};
use crate::config::{Config, DigestEmpty, Project, Urgency};
use crate::i18n::{tr, Lang};
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Maior mensagem da pessoa guardada (o limite do Telegram).
pub const MAX_PERSON_CHARS: usize = 4096;
/// Silêncio do comando sem número, em horas.
pub const SILENCE_DEFAULT_HOURS: u64 = 1;
/// Maior silêncio pelo comando, em horas.
pub const SILENCE_MAX_HOURS: u64 = 24;
/// Projetos listados quando o Nanty pede para escolher.
const MAX_LISTED_PROJECTS: usize = 10;
/// Cartões de confirmação de uma resposta do modelo.
const MAX_REQUESTED_CARDS: usize = turn::MAX_MODEL_ACTIONS;

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
    let scrubbed = secrets.scrub_exact(raw.trim());
    let text: String = scrubbed.trim().chars().take(MAX_PERSON_CHARS).collect();
    (!text.trim().is_empty()).then_some(text)
}

/// Grava a mensagem da pessoa na conversa (redigida) e a devolve. `None` sem texto.
pub fn person_message(nanty: &Nanty, secrets: &SecretSet, channel: Channel, raw: &str) -> Option<ConversationMessage> {
    person_message_with(nanty, secrets, channel, Some(raw), false, None)
}

/// Como [`person_message`], com a marca da nota de voz (T09): o texto é a transcrição (redigida do
/// mesmo jeito) e o áudio é a nota guardada. A nota que não virou texto entra sem texto; sem a
/// marca, `None` sem texto.
pub fn person_message_with(
    nanty: &Nanty,
    secrets: &SecretSet,
    channel: Channel,
    raw: Option<&str>,
    voice: bool,
    audio: Option<AudioRef>,
) -> Option<ConversationMessage> {
    let text = raw.and_then(|r| redact(secrets, r));
    if text.is_none() && !voice {
        return None;
    }
    let message = ConversationMessage {
        id: super::new_id(),
        at_ms: nanty.now().epoch_ms,
        from: Speaker::Person,
        channel,
        project_id: None,
        text,
        code: None,
        params: Map::new(),
        lang: None,
        card_ids: Vec::new(),
        actions: Vec::new(),
        reply_to: None,
        voice,
        audio,
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

/// A resposta à mensagem da pessoa: o comando (sem modelo) ou o texto livre (com o modelo). Sai pelo
/// canal da mensagem. Devolve a resposta gravada.
pub async fn reply(state: &AppState, person: &ConversationMessage) -> Option<ConversationMessage> {
    let nanty = Arc::clone(state.nanty());
    let text = person.text.clone()?;
    let cfg = state.config().read().await.clone();
    let lang = nanty_lang(&cfg.nanty);
    let channel = person.channel;
    let (kind, mut message) = match parse(&text) {
        Parsed::Command(command, args) => (command.as_str(), command_reply(state, &nanty, &cfg, lang, command, args, channel).await),
        Parsed::Unknown => ("unknown", fixed(&nanty, "nanty.chat.unknown_command", with_commands(lang))),
        Parsed::Text => ("text", converse(state, &nanty, &cfg, lang, &text, channel).await),
    };
    message.channel = channel;
    message.reply_to = Some(person.id.clone());
    let mut entry = JournalEntry::new(nanty.now().epoch_ms, JournalKind::Conversation, "chat.reply");
    entry.project_id = message.project_id.clone();
    entry.params.insert("input".into(), json!(kind));
    entry.params.insert("channel".into(), json!(channel));
    entry.params.insert("cards".into(), json!(message.card_ids.len()));
    if let Some(code) = &message.code {
        entry.params.insert("reply".into(), json!(code));
    }
    nanty.record(&entry);
    // T09: a quem falou por voz no Telegram, com a voz ligada, a resposta também vai em voz.
    super::voice::attach_speech(state, &nanty, person, &mut message).await;
    let n = Arc::clone(&nanty);
    let saved = message.clone();
    let _ = tokio::task::spawn_blocking(move || n.push_message(&saved)).await;
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

fn round_cents(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
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
        Command::Status => status(nanty, cfg),
        Command::Summary => summary(state, nanty, cfg).await,
        Command::Pause => pause(state, nanty, cfg, args, channel).await,
        Command::Silence => silence(nanty, args),
        Command::Help => fixed(nanty, "nanty.chat.help", with_commands(lang)),
    }
}

/// O silêncio de tudo ainda valendo agora (o mais longo).
fn global_silence(nanty: &Nanty, now_ms: u64) -> Option<u64> {
    nanty
        .snapshot()
        .silences
        .iter()
        .filter(|s| s.project_id.is_none() && s.group.is_none() && s.until_ms > now_ms)
        .map(|s| s.until_ms)
        .max()
}

/// `status`: o que espera a pessoa, o que foi feito hoje, o previsto, o silêncio e o orçamento.
fn status(nanty: &Nanty, cfg: &Config) -> ConversationMessage {
    let now = nanty.now();
    let snap = nanty.snapshot();
    let waiting = snap.cards.iter().filter(|c| c.expires_at_ms > now.epoch_ms && c.urgency != Some(Urgency::Digest)).count();
    let start = digest::day_start_ms(&now);
    let mut params = Map::new();
    params.insert("waiting".into(), json!(waiting));
    params.insert("done".into(), json!(snap.done.iter().filter(|d| d.at_ms >= start).count()));
    // O previsto conta também o resumo da manhã, que está sempre agendado: o mesmo número da seção
    // "Previsto" da janela (T10).
    params.insert("planned".into(), json!(snap.planned.len() + 1));
    // As linhas extras vão em `params.lines`, cada uma `{code, params}`, como no resumo da manhã.
    let line = |code: &str, params: Value| json!({ "code": code, "params": params });
    let mut lines = Vec::new();
    if let Some(until) = global_silence(nanty, now.epoch_ms) {
        lines.push(line("nanty.chat.status_silence", json!({ "until": hhmm(until) })));
    }
    if turn::model_route(cfg).is_none() {
        lines.push(line("nanty.chat.status_no_model", json!({})));
    } else {
        let view = turn::budget_view(cfg, &snap.spend_on(&now.day_key()));
        lines.push(match view.mode {
            turn::BudgetMode::Usd => line(
                "nanty.chat.status_budget_usd",
                json!({ "spent": round_cents(view.spent_usd), "budget": round_cents(view.budget_usd) }),
            ),
            turn::BudgetMode::Tokens => {
                line("nanty.chat.status_budget_tokens", json!({ "tokens": view.tokens, "budget": view.budget_tokens }))
            }
        });
    }
    params.insert("lines".into(), Value::Array(lines));
    fixed(nanty, "nanty.chat.status", params)
}

/// `resumo`: os fatos do resumo da manhã, agora, sem modelo e sem consumir os avisos guardados.
async fn summary(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config) -> ConversationMessage {
    let now = nanty.now();
    let facts = digest::gather(state, nanty, &now).await;
    let mut short = cfg.nanty.clone();
    short.digest_empty = DigestEmpty::Short;
    match digest::compose(&facts, &short, super::new_id(), now.epoch_ms) {
        Some(mut message) => {
            let code = if message.code.as_deref() == Some(digest::QUIET) { "nanty.chat.summary_quiet" } else { "nanty.chat.summary" };
            message.code = Some(code.to_string());
            message
        }
        None => fixed(nanty, "nanty.chat.summary_quiet", param("projects", facts.projects)),
    }
}

/// Os projetos que o Nanty observa.
fn observed(cfg: &Config) -> Vec<&Project> {
    cfg.projects.iter().filter(|p| cfg.nanty.observes(&p.id)).collect()
}

/// "Escolha um projeto": os nomes, até [`MAX_LISTED_PROJECTS`].
fn choose(nanty: &Nanty, projects: &[&Project]) -> ConversationMessage {
    if projects.is_empty() {
        return fixed(nanty, "nanty.chat.no_projects", Map::new());
    }
    let names: Vec<&str> = projects.iter().take(MAX_LISTED_PROJECTS).map(|p| p.name.as_str()).collect();
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
            many => return choose(nanty, many),
        }
    } else {
        match cite(&projects, args) {
            Cited::One(p) => p,
            Cited::Ambiguous(list) => return choose(nanty, &list),
            Cited::None | Cited::Unknown => return choose(nanty, &projects),
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
/// [`SILENCE_MAX_HOURS`]); `0` (ou `off`, `desligar`) desliga.
fn silence(nanty: &Nanty, args: &str) -> ConversationMessage {
    let arg = args.trim().to_lowercase();
    let hours = match arg.as_str() {
        "" => Some(SILENCE_DEFAULT_HOURS),
        "off" | "desligar" => Some(0),
        other => other.parse::<u64>().ok(),
    };
    let Some(hours) = hours.filter(|h| *h <= SILENCE_MAX_HOURS) else {
        return fixed(nanty, "nanty.chat.silence_usage", param("max", SILENCE_MAX_HOURS));
    };
    let now_ms = nanty.now().epoch_ms;
    let until = now_ms.saturating_add(hours.saturating_mul(60 * 60 * 1000));
    nanty.update(|st| {
        st.silences.retain(|s| s.project_id.is_some() || s.group.is_some());
        if hours > 0 {
            st.silences.push(Silence { project_id: None, group: None, until_ms: until });
        }
        true
    });
    if hours == 0 {
        fixed(nanty, "nanty.chat.silence_off", Map::new())
    } else {
        fixed(nanty, "nanty.chat.silence_on", param("until", hhmm(until)))
    }
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

/// O texto em palavras minúsculas, cercado de espaços: `" olha o projeto loja "`.
fn words(text: &str) -> String {
    let flat: String = text.chars().map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { ' ' }).collect();
    format!(" {} ", flat.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Palavras depois de "projeto" que não são nome (`o projeto que falhou`).
const NOT_A_NAME: [&str; 24] = [
    "que", "de", "do", "da", "dos", "das", "no", "na", "em", "e", "ou", "esta", "está", "agora", "pra", "para", "that",
    "is", "the", "of", "in", "on", "and", "now",
];

/// O projeto citado: pelo nome (ou pelo id), como palavras inteiras. Um nome contido em outro citado
/// junto (`Loja` dentro de `Loja Nova`) não conta à parte.
pub fn cite<'a>(projects: &[&'a Project], text: &str) -> Cited<'a> {
    let hay = words(text);
    let mut found: Vec<(&'a Project, String)> = Vec::new();
    for p in projects {
        let name = words(&p.name);
        let id = words(&p.id);
        let hit = [name.as_str(), id.as_str()].iter().any(|w| w.trim().len() > 1 && hay.contains(*w));
        if hit && !found.iter().any(|(f, _)| f.id == p.id) {
            found.push((p, name));
        }
    }
    let names: Vec<String> = found.iter().map(|(_, n)| n.clone()).collect();
    found.retain(|(_, name)| !names.iter().any(|other| other != name && other.contains(name.as_str())));
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

/// A entrada do modelo na conversa: a mensagem da pessoa (já redigida), os sinais estruturados, as
/// contagens, os ids que ele pode propor e, se a pessoa citou um projeto, a leitura dele (com o
/// conteúdo dos arquivos em `project.untrusted`).
#[derive(Debug, Serialize)]
struct ChatInput<'a> {
    kind: TurnKind,
    message: &'a str,
    signals: Vec<SignalFact>,
    counts: BTreeMap<&'static str, u64>,
    allowed_actions: Vec<ActionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<Value>,
}

/// O texto livre: confere o projeto citado, monta a entrada, chama o modelo pela fila única e
/// transforma cada id proposto num cartão de confirmação. Sem modelo, sem orçamento ou com resposta
/// inválida, a resposta é o texto fixo.
async fn converse(
    state: &AppState,
    nanty: &Arc<Nanty>,
    cfg: &Config,
    lang: Lang,
    text: &str,
    channel: Channel,
) -> ConversationMessage {
    if turn::model_route(cfg).is_none() {
        return fixed(nanty, "nanty.chat.no_model", with_commands(lang));
    }
    let projects = observed(cfg);
    let project = match cite(&projects, text) {
        Cited::None => None,
        Cited::One(p) => Some(p.clone()),
        Cited::Unknown => return choose(nanty, &projects),
        Cited::Ambiguous(list) => return choose(nanty, &list),
    };
    let pid = project.as_ref().map(|p| p.id.clone());
    let snap = nanty.snapshot();
    let signals: Vec<SignalFact> = snap
        .alerts
        .values()
        .filter(|a| pid.is_none() || a.project_id.is_none() || a.project_id == pid)
        .filter_map(SignalFact::of_alert)
        .take(turn::MAX_SIGNALS_IN)
        .collect();
    let offers = act::requestable(state, cfg, &snap, pid.as_deref());
    let mut allowed: Vec<ActionId> = Vec::new();
    for offer in &offers {
        if !allowed.contains(&offer.action) {
            allowed.push(offer.action);
        }
    }
    let now_ms = nanty.now().epoch_ms;
    let mut counts = BTreeMap::new();
    counts.insert("projects", projects.len() as u64);
    counts.insert("running", projects.iter().filter(|p| state.runs().active(&p.id).is_some()).count() as u64);
    counts.insert("open_cards", snap.cards.iter().filter(|c| c.expires_at_ms > now_ms).count() as u64);
    let data = match &project {
        Some(p) => Some(super::read::project_view(state, cfg, p).await),
        None => None,
    };
    let input = ChatInput { kind: TurnKind::Conversation, message: text, signals, counts, allowed_actions: allowed.clone(), project: data };
    let call = Call {
        kind: TurnKind::Conversation,
        system: crate::agents::prompts::nanty_chat_system(lang),
        user: serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string()),
        allowed: &allowed,
        signals: input.signals.len(),
    };
    let reply = match turn::run_call(state, nanty, &call).await {
        TurnOutcome::Reply(reply) => reply,
        TurnOutcome::Skipped(reason) => {
            let code = match reason {
                "budget" => "nanty.chat.budget",
                "busy" => "nanty.chat.busy",
                "no_model" => "nanty.chat.no_model",
                _ => "nanty.chat.failed",
            };
            return fixed(nanty, code, with_commands(lang));
        }
    };
    // Cada id proposto vira um cartão de confirmação: nada é executado aqui.
    let mut card_ids = Vec::new();
    let mut actions = Vec::new();
    for id in &reply.actions {
        for offer in offers.iter().filter(|o| o.action == *id) {
            if card_ids.len() >= MAX_REQUESTED_CARDS {
                break;
            }
            if let Ok(card) = act::request_card(state, offer, channel).await {
                if !card_ids.contains(&card) {
                    card_ids.push(card);
                }
                let name = id.as_str().to_string();
                if !actions.contains(&name) {
                    actions.push(name);
                }
            }
        }
    }
    let secrets = SecretSet::from_config(&*state.config().read().await);
    let text = Some(format::clean(&secrets, &reply.summary)).filter(|t| !t.is_empty());
    let mut message = fixed(nanty, "nanty.chat.confirm", Map::new());
    match (card_ids.is_empty(), &text) {
        (true, Some(_)) => message.code = None,
        (true, None) => return fixed(nanty, "nanty.chat.failed", with_commands(lang)),
        (false, _) => {}
    }
    message.text = text;
    message.lang = Some(lang.tag().to_string());
    message.project_id = pid;
    message.card_ids = card_ids;
    message.actions = actions;
    message
}

#[cfg(test)]
#[path = "t08_tests.rs"]
pub(crate) mod tests;
