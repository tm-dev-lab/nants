//! Ferramentas da conversa do Nanty (spec 139 T03, C-04; as ações e o silêncio, T04, G3, C-07 e
//! C-08).
//!
//! - **Saída em frases curtas do dicionário,** no idioma do Nanty, mais os ids. Zeros e valores
//!   padrão ficam de fora (`ProjectFacts::status_facts`).
//! - **Ids validados:** `project_status`, `project_details` e `propose_action` só aceitam projeto
//!   observado ([`super::chat::observed`]); um id excluído ou desconhecido recebe a frase de projeto
//!   desconhecido, sem nenhuma leitura.
//! - **Grau de confiança declarado:** `project_status` é confiável (a fonte única da T02,
//!   [`super::observe::project_facts`]). `project_details` é não confiável: o snapshot e o relatório
//!   ([`super::read::project_view`], já redigidos) vão num envelope de dado não confiável, com a
//!   instrução de que o estado vale sobre o texto. Ler conteúdo assim contamina o turno
//!   ([`Session::tainted`]), e há um teto de leituras por mensagem (`[nanty.chat]`).
//! - **Ações só por proposta (T04):** `propose_action` só aceita o que [`act::requestable`] oferece
//!   agora para aquele projeto; o servidor cria o cartão ([`act::request_card`]) e o resultado diz
//!   ao modelo que só o clique executa. Nada executa aqui, nem a ação automática.
//! - **Silêncio (T04):** `set_silence` liga o silêncio de tudo direto, com a confirmação do servidor
//!   ([`super::silence`]); num turno contaminado, vira o cartão de confirmação. Nenhuma ferramenta
//!   muda canal, token ou destinatário.
//! - **Abrir o Nants (spec 140 T03, C-30):** `open_nants` guarda o pedido na sessão, e o servidor
//!   acrescenta o caminho à resposta ([`super::link`]): o link, a tela ou o motivo de não dar. O texto
//!   do modelo não leva link, e o resultado da ferramenta não leva o endereço.
//! - **Ferramentas por modo (spec 140 T04, C-06):** [`Mode`] lista o que existe em cada modo, e o
//!   despachante confere antes de tudo: chamar pelo nome uma ferramenta não oferecida não executa nada.
//! - **Memória (spec 140 T07):** `remember`, `recall` e `forget` ([`memory::tools`]) só existem na
//!   conversa; num turno contaminado, gravar e apagar viram o cartão de confirmação.
//! - **Leitura profunda (spec 140 T10, G5):** `agent_log`, `sdd_docs`, `project_files` e
//!   `git_history` ([`deep`]) existem na conversa e, desde a T12, na análise. O despachante confere o projeto, os parâmetros e o
//!   isolamento (C-16: outro projeto só no mesmo pedido do primeiro, antes de o modelo ver conteúdo; a
//!   regra vale também para `project_details`), e toda leitura contamina o turno com o tipo e o projeto.
//! - **Análise presa ao projeto do aviso (spec 140 T12, C-11):** o modo análise ganha a leitura
//!   profunda, e o despachante recusa qualquer `project_id` que não seja o da [`Session::pinned`].
//! - **Pensar mais (spec 140 T13, C-25):** `deep_think` ([`super::think`]) só existe na conversa e
//!   só sobe o esforço das rodadas seguintes da mensagem, uma vez, dentro do orçamento.
//! - **Levar ao Arquiteto (spec 140 T15, C-15):** `propose_to_architect` ([`super::draft`]) só existe
//!   na conversa e só cria o cartão com o texto congelado; o clique deixa um rascunho na caixa do
//!   Arquiteto, e quem envia é a pessoa.
//! - **Tudo sai redigido:** o redator único ([`redact::scrub`]) passa sobre o texto de cada resultado.

use serde_json::{json, Map, Value};

use super::act;
use super::actions::{self, ActionKind};
use super::agent::{Session, SilenceEffect};
use super::link;
use super::observe::{Phase, ProjectFacts};
use super::provenance::TaintKind;
use super::signals::SignalKind;
use super::silence::{self, Request};
use super::store::DecidedBy;
use super::{chat, clock, deep, digest, files, memory, redact, render, turn, Nanty};
use crate::agents::llm_client::ToolCall;
use crate::config::{Config, Project, GB_BYTES};
use crate::i18n::Lang;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Estado de um projeto (confiável).
pub const PROJECT_STATUS: &str = "project_status";
/// O que foi lido dos arquivos de um projeto (não confiável).
pub const PROJECT_DETAILS: &str = "project_details";
/// A máquina e o Nanty.
pub const SYSTEM_STATUS: &str = "system_status";
/// O que o Nanty fez e decidiu hoje.
pub const NANTY_ACTIVITY: &str = "nanty_activity";
/// Propor uma ação do catálogo: o servidor cria o cartão (T04).
pub const PROPOSE_ACTION: &str = "propose_action";
/// Silenciar os avisos por algumas horas (T04).
pub const SET_SILENCE: &str = "set_silence";
/// Levar a pessoa ao Nants (spec 140 T03): o servidor acrescenta o caminho.
pub const OPEN_NANTS: &str = "open_nants";
/// Guardar na memória de longo prazo (spec 140 T07).
pub const REMEMBER: &str = "remember";
/// Buscar na memória de longo prazo (spec 140 T07).
pub const RECALL: &str = "recall";
/// Apagar da memória de longo prazo (spec 140 T07).
pub const FORGET: &str = "forget";
/// Levar uma ideia ao Arquiteto como rascunho (spec 140 T15): só na conversa.
pub const PROPOSE_TO_ARCHITECT: &str = super::draft::PROPOSE_TO_ARCHITECT;
/// Pensar de novo com esforço maior (spec 140 T13): só na conversa.
pub const DEEP_THINK: &str = super::think::DEEP_THINK;
/// Cartões de ação por mensagem, no máximo.
pub const MAX_PROPOSED_CARDS: usize = turn::MAX_MODEL_ACTIONS;
/// Abertura e fecho do envelope de dado não confiável.
pub const UNTRUSTED_OPEN: &str = "<untrusted_data>";
pub const UNTRUSTED_CLOSE: &str = "</untrusted_data>";
/// Teto do nome do projeto, em bytes.
pub const NAME_MAX_BYTES: usize = 100;
/// Linhas do que foi feito e decidido hoje.
const MAX_ACTIVITY: usize = 20;

/// O modo do turno (spec 140 T04, C-06): decide quais ferramentas existem nele.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// A pessoa escreveu: todas as ferramentas da conversa.
    #[default]
    Conversation,
    /// Turno de análise, sem pessoa presente: só leitura, do projeto do aviso (spec 140 T12).
    Analysis,
}

impl Mode {
    /// As ferramentas do modo. Quem cria uma ferramenta nova decide aqui em que modo ela existe.
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Mode::Conversation => &[
                PROJECT_STATUS,
                PROJECT_DETAILS,
                deep::AGENT_LOG,
                deep::SDD_DOCS,
                deep::PROJECT_FILES,
                deep::GIT_HISTORY,
                SYSTEM_STATUS,
                NANTY_ACTIVITY,
                PROPOSE_ACTION,
                OPEN_NANTS,
                SET_SILENCE,
                REMEMBER,
                RECALL,
                FORGET,
                DEEP_THINK,
                PROPOSE_TO_ARCHITECT,
            ],
            // Spec 140 T12: a leitura profunda também, só do projeto do aviso ([`Session::pinned`]).
            Mode::Analysis => &[PROJECT_STATUS, PROJECT_DETAILS, deep::AGENT_LOG, deep::SDD_DOCS, deep::PROJECT_FILES, deep::GIT_HISTORY],
        }
    }

    /// A ferramenta existe neste modo.
    pub fn allows(self, name: &str) -> bool {
        self.tools().contains(&name)
    }
}

/// Uma linha: o código do dicionário e os parâmetros.
pub type Line = (String, Map<String, Value>);

fn line(code: &str, params: &[(&str, Value)]) -> Line {
    (code.to_string(), params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect())
}

/// As linhas no idioma, uma por linha.
pub fn render_lines(lang: Lang, lines: &[Line]) -> String {
    lines.iter().map(|(code, params)| render::code_text(lang, code, params)).collect::<Vec<_>>().join("\n")
}

/// Hora local `HH:MM`.
pub fn hhmm(at_ms: u64) -> String {
    clock::local_time_at(at_ms).map(|t| format!("{:02}:{:02}", t.hour, t.minute)).unwrap_or_default()
}

/// O nome do projeto como vai ao modelo: sem controle, redigido e cortado.
pub fn project_name(secrets: &SecretSet, project: &Project) -> String {
    crate::server::snapshot::clean_field(secrets, &project.name, NAME_MAX_BYTES)
}

/// O código da frase de uma fase.
pub fn phase_code(phase: Phase) -> String {
    format!("nanty.phase.{}", phase.as_str())
}

/// As linhas do estado de um projeto. `full`: com os códigos internos e o custo (a ferramenta, para o
/// modelo); sem ele, só as linhas que a janela também traduz (a reserva sem modelo). Spec 139 T12: o
/// id da spec fica de fora nos dois: na rodada real, o modelo o repetia na resposta, e ele não diz
/// nada a quem lê. A frase dele continua nos dicionários para as mensagens já gravadas.
pub fn status_lines(facts: &ProjectFacts, full: bool) -> Vec<Line> {
    let f = facts.status_facts();
    let mut out = vec![line(&phase_code(facts.phase()), &[])];
    // Spec 140 T04 (C-05): só o id na forma do SDD; o resto é texto escrito no projeto.
    if let Some(v) = f.get("task_id").filter(|v| v.as_str().is_some_and(turn::is_task_id)) {
        out.push(line("nanty.status.task", &[("task", v.clone())]));
    }
    if let (Some(done), Some(total)) = (f.get("done"), f.get("total")) {
        out.push(progress_line(done, total));
    }
    if let Some(since) = f.get("since") {
        out.push(line("nanty.status.since", &[("since", since.clone())]));
    }
    if !full {
        return out;
    }
    // Spec 139 T06: os códigos internos viram frases do dicionário; um código fora das listas fixas
    // (o de uma tarefa escrita à mão, por exemplo) fica de fora, nunca vai cru.
    if let Some(v) = f.get("reason").and_then(Value::as_str).filter(|v| crate::agents::runner::BLOCK_CODES.contains(v)) {
        out.push(line(&format!("nanty.reason.{v}"), &[]));
    }
    if let Some(v) = f.get("attempts").filter(|v| v.as_u64().is_some_and(|n| n > 0)) {
        match f.get("max_attempts") {
            Some(max) => out.push(line("nanty.status.attempts_of", &[("count", v.clone()), ("max", max.clone())])),
            None => out.push(line("nanty.status.attempts", &[("count", v.clone())])),
        }
    }
    let stop = f.get("stop").and_then(Value::as_str).filter(|v| crate::server::attention::PROVIDER_STOP_CODES.contains(v));
    if let Some(v) = stop.and_then(|v| v.strip_prefix("run.")) {
        out.push(line(&format!("nanty.stop.{v}"), &[]));
        if let Some(role) = f.get("role").and_then(Value::as_str).filter(|r| matches!(*r, "architect" | "developer" | "reviewer")) {
            out.push(line(&format!("nanty.status.stop_role.{role}"), &[]));
        }
    }
    let waiting: Vec<&str> = f.get("waiting").and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    for item in waiting {
        let code = format!("nanty.status.waiting.{item}");
        let permissions = f.get("permissions").cloned().unwrap_or(json!(0));
        out.push(line(&code, &[("count", permissions)]));
    }
    if let Some(v) = f.get("cost_usd") {
        out.push(line("nanty.status.cost", &[("usd", v.clone())]));
    }
    if let Some(v) = f.get("cost_alerts") {
        out.push(line("nanty.status.cost_alerts", &[("count", v.clone())]));
    }
    out
}

/// A linha do progresso. Spec 139 T06: nenhuma pronta não vira "0 de N"; a concordância segue o
/// `count` (as prontas, ou o total quando nenhuma está). Spec 139 T13: a tarefa única pronta tem
/// frase própria, sem o "1 de 1".
fn progress_line(done: &Value, total: &Value) -> Line {
    match (done.as_u64(), total.as_u64()) {
        (Some(0), _) => line("nanty.status.progress_none", &[("count", total.clone())]),
        (Some(1), Some(1)) => line("nanty.status.progress_only", &[]),
        _ => line("nanty.status.progress", &[("count", done.clone()), ("total", total.clone())]),
    }
}

fn note_read(session: &mut Session, id: &str) {
    if !session.read.iter().any(|r| r == id) {
        session.read.push(id.to_string());
    }
}

/// Executa uma chamada do modelo e devolve o texto do resultado, redigido. A sessão guarda o que o
/// turno fez: os projetos consultados (para a mensagem ficar ligada ao projeto), a contaminação, as
/// leituras não confiáveis, os cartões e o silêncio.
pub async fn call(
    state: &AppState,
    nanty: &Nanty,
    cfg: &Config,
    lang: Lang,
    secrets: &SecretSet,
    call: &ToolCall,
    session: &mut Session,
) -> String {
    // Spec 140 T04 (C-06): a lista oferecida não restringe a execução; a do modo, sim.
    if !session.mode.allows(&call.name) {
        return redact::scrub(secrets, &unknown_tool(lang, &call.name));
    }
    // Spec 140 T12 (C-11): na análise, só o projeto do aviso, em toda ferramenta, antes de qualquer
    // leitura. Outro id recebe a mesma frase do segundo projeto da conversa.
    if let Some(pin) = &session.pinned {
        let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
        if args.get("project_id").and_then(Value::as_str).map(str::trim).is_some_and(|id| id != pin) {
            return crate::i18n::tr(lang, "nanty.tool.other_project", &[]);
        }
    }
    let out = match call.name.as_str() {
        PROJECT_STATUS => match project_arg(cfg, lang, &call.arguments) {
            Ok(project) => {
                note_read(session, &project.id);
                project_status(state, cfg, lang, secrets, project).await
            }
            Err(text) => text,
        },
        PROJECT_DETAILS => match project_arg(cfg, lang, &call.arguments) {
            // C-07: o teto de leituras não confiáveis por mensagem.
            Ok(_) if session.details >= cfg.nanty.chat.details() => {
                crate::i18n::tr(lang, "nanty.tool.details_limit", &[("max", &cfg.nanty.chat.details().to_string())])
            }
            // Spec 140 T10 (C-16): outro projeto, só no mesmo pedido do primeiro.
            Ok(project) if !session.admit(&project.id) => crate::i18n::tr(lang, "nanty.tool.other_project", &[]),
            Ok(project) => {
                note_read(session, &project.id);
                session.details += 1;
                // Daqui em diante, o turno tem dado não confiável no contexto.
                session.tainted.read(TaintKind::ProjectFile, Some(&project.id));
                project_details(state, cfg, lang, project).await
            }
            Err(text) => text,
        },
        SYSTEM_STATUS => system_status(nanty, cfg, lang).await,
        NANTY_ACTIVITY => activity(nanty, cfg, lang, secrets),
        PROPOSE_ACTION => propose_action(state, nanty, cfg, lang, secrets, &call.arguments, session).await,
        OPEN_NANTS => open_nants(state, cfg, lang, secrets, &call.arguments, session),
        SET_SILENCE => set_silence(nanty, cfg, lang, &call.arguments, session),
        // Spec 140 T07: a memória de longo prazo; nada daqui muda ação, canal ou token.
        REMEMBER => memory::tools::remember(nanty, cfg, lang, secrets, &call.arguments, session),
        RECALL => memory::tools::recall(nanty, cfg, lang, &call.arguments, session),
        FORGET => memory::tools::forget(nanty, cfg, lang, &call.arguments, session),
        // Spec 140 T13 (C-25): só muda o esforço das próximas rodadas desta mensagem.
        DEEP_THINK => super::think::call(nanty, cfg, lang, session),
        // Spec 140 T15 (C-15): só o cartão; o clique deixa o rascunho, e a pessoa envia.
        PROPOSE_TO_ARCHITECT => super::draft::propose(nanty, cfg, lang, secrets, &call.arguments, session),
        // Spec 140 T10: a leitura profunda, só de leitura.
        name if deep::TOOLS.contains(&name) => deep_read(state, cfg, lang, secrets, call, session).await,
        other => unknown_tool(lang, other),
    };
    // Spec 140 T04 (C-09): toda leitura sai pelo redator único, inclusive as de leitura profunda.
    redact::scrub(secrets, &out)
}

/// Uma leitura profunda ([`deep`], spec 140 T10): o projeto observado, os parâmetros conferidos e o
/// isolamento entre projetos (C-16) antes de tocar no disco. Toda leitura admitida contamina o turno
/// com o tipo e o projeto (revê a C-13 da 138), e o teto de leitura vale para o turno todo.
async fn deep_read(state: &AppState, cfg: &Config, lang: Lang, secrets: &SecretSet, call: &ToolCall, session: &mut Session) -> String {
    let project = match project_arg(cfg, lang, &call.arguments) {
        Ok(project) => project,
        Err(text) => return text,
    };
    let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
    let Some(request) = deep::Request::parse(&call.name, &args) else {
        return crate::i18n::tr(lang, "nanty.tool.deep_args", &[]);
    };
    if !session.admit(&project.id) {
        return crate::i18n::tr(lang, "nanty.tool.other_project", &[]);
    }
    note_read(session, &project.id);
    session.tainted.read(request.taint(), Some(&project.id));
    let mut budget = session.read_budget.unwrap_or_else(|| files::Budget::new(&cfg.nanty.read));
    let out = deep::read(state, cfg, lang, secrets, project, request, &mut budget).await;
    session.read_budget = Some(budget);
    out
}

/// A recusa de uma ferramenta que não existe ou que o modo não oferece: a mesma frase, porque para o
/// modelo as duas coisas são iguais.
fn unknown_tool(lang: Lang, name: &str) -> String {
    crate::i18n::tr(lang, "nanty.tool.unknown_tool", &[("name", shown(name))])
}

/// O projeto do argumento `project_id`, só entre os observados. `Err` com a frase para o modelo.
fn project_arg<'a>(cfg: &'a Config, lang: Lang, arguments: &str) -> Result<&'a Project, String> {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let Some(id) = args.get("project_id").and_then(Value::as_str).map(str::trim).filter(|id| !id.is_empty()) else {
        return Err(crate::i18n::tr(lang, "nanty.tool.bad_args", &[]));
    };
    chat::observed(cfg).into_iter().find(|p| p.id == id).ok_or_else(|| {
        let shown = if turn::is_code(id) { id } else { "?" };
        crate::i18n::tr(lang, "nanty.tool.unknown_project", &[("id", shown)])
    })
}

/// O texto de um id vindo do modelo, só se tiver forma de código (nunca o texto cru dele).
fn shown(raw: &str) -> &str {
    if turn::is_code(raw) {
        raw
    } else {
        "?"
    }
}

/// `propose_action(action_id, project_id)` (T04): só o que [`act::requestable`] oferece agora para
/// aquele projeto observado (ou, sem projeto, para a máquina). O servidor cria o cartão; nada executa
/// sem o clique, nem a ação automática.
async fn propose_action(
    state: &AppState,
    nanty: &Nanty,
    cfg: &Config,
    lang: Lang,
    secrets: &SecretSet,
    arguments: &str,
    session: &mut Session,
) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let raw = args.get("action_id").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let tr = |code: &str, params: &[(&str, &str)]| crate::i18n::tr(lang, code, params);
    // Spec 140 T03: abrir o Nants não é cartão. O pedido certo, pelo caminho errado, ainda vale.
    if raw == OPEN_NANTS {
        return open_nants(state, cfg, lang, secrets, arguments, session);
    }
    // Só as ações que o Nanty executa: opções do cartão, links, silêncio e a faixa "só a pessoa"
    // nunca vêm por aqui.
    let Some(action) = actions::parse(raw).filter(|a| a.entry().kind == ActionKind::Executes) else {
        return tr("nanty.tool.action_unknown", &[("action", shown(raw))]);
    };
    let project = match args.get("project_id").and_then(Value::as_str).map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => match chat::observed(cfg).into_iter().find(|p| p.id == id) {
            Some(p) => Some(p.id.clone()),
            None => return tr("nanty.tool.unknown_project", &[("id", shown(id))]),
        },
        None => None,
    };
    let offers = act::requestable(state, cfg, &nanty.snapshot(), project.as_deref());
    let mut offered: Vec<&str> = offers.iter().filter(|o| o.project_id == project).map(|o| o.action.as_str()).collect();
    offered.sort_unstable();
    offered.dedup();
    let Some(offer) = offers.iter().find(|o| o.action == action && o.project_id == project) else {
        let list = if offered.is_empty() { tr("nanty.tool.none", &[]) } else { offered.join(", ") };
        return tr("nanty.tool.action_not_offered", &[("action", action.as_str()), ("offered", &list)]);
    };
    // Spec 139 T06: o nome da ação, do dicionário, e não o id.
    let label = tr(&action.label_code(), &[]);
    if session.cards.len() >= MAX_PROPOSED_CARDS {
        return tr("nanty.tool.cards_limit", &[("max", &MAX_PROPOSED_CARDS.to_string())]);
    }
    match act::request_card(state, offer, session.channel).await {
        Ok(card) => {
            if let Some(p) = &project {
                note_read(session, p);
            }
            if !session.cards.contains(&card) {
                session.cards.push(card.clone());
            }
            if !session.actions.iter().any(|a| a == action.as_str()) {
                session.actions.push(action.as_str().to_string());
            }
            tr("nanty.tool.card_created", &[("action", &label), ("card", &card)])
        }
        Err(act::NoCard::Off) => tr("nanty.tool.action_off", &[("action", &label)]),
        Err(act::NoCard::Gone) => tr("nanty.tool.action_gone", &[("action", &label)]),
        Err(act::NoCard::Unavailable) => tr("nanty.tool.action_unavailable", &[]),
    }
}

/// `open_nants(project_id?)` (spec 140 T03, C-30): guarda o caminho na sessão, e o servidor o acrescenta
/// à resposta. O resultado diz ao modelo o que a pessoa vai receber, sem o endereço.
fn open_nants(state: &AppState, cfg: &Config, lang: Lang, secrets: &SecretSet, arguments: &str, session: &mut Session) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let project = match args.get("project_id").and_then(Value::as_str).map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => match chat::observed(cfg).into_iter().find(|p| p.id == id) {
            Some(p) => Some(p),
            None => return crate::i18n::tr(lang, "nanty.tool.unknown_project", &[("id", shown(id))]),
        },
        None => None,
    };
    if let Some(p) = project {
        note_read(session, &p.id);
    }
    let open = link::resolve(state, cfg, secrets, session.channel, project);
    let text = crate::i18n::tr(lang, open.tool_code(), &[]);
    session.open = Some(open);
    text
}

/// `set_silence(hours | until)` (T04, C-08; `until`, spec 139 T12: a próxima ocorrência de uma hora
/// `HH:MM`, que vale sobre `hours`): de 1 a [`chat::SILENCE_MAX_HOURS`] horas liga o silêncio de
/// tudo direto, com a confirmação do servidor e o botão para desligar; zero desliga. Num turno
/// contaminado (C-07), ligar vira o cartão de confirmação.
fn set_silence(nanty: &Nanty, cfg: &Config, lang: Lang, arguments: &str, session: &mut Session) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let max = chat::SILENCE_MAX_HOURS;
    let request = match args.get("until").filter(|v| !v.is_null()) {
        Some(clock) => clock.as_str().and_then(silence::parse_clock).map(|(hour, minute)| Request::Clock(hour, minute)),
        None => args.get("hours").and_then(Value::as_u64).filter(|h| *h <= max).map(Request::Hours),
    };
    let Some(request) = request else {
        return crate::i18n::tr(lang, "nanty.tool.silence_args", &[("max", &max.to_string())]);
    };
    // Os cartões do silêncio trocados nesta chamada saem da lista da mensagem.
    let keep_open = |session: &mut Session| {
        let snap = nanty.snapshot();
        session.cards.retain(|id| snap.cards.iter().any(|c| &c.id == id));
    };
    if request == Request::Hours(0) {
        // Desligar só devolve os avisos: vale direto, mesmo num turno contaminado.
        super::silence::end(nanty, session.channel);
        keep_open(session);
        session.silence = Some(SilenceEffect::Ended);
        return crate::i18n::tr(lang, "nanty.tool.silence_off", &[]);
    }
    let (until, card, code) = if session.is_tainted() {
        let (until, card) = silence::propose(nanty, cfg, request, session.channel);
        session.silence = Some(SilenceEffect::Proposed(until));
        (until, card, "nanty.tool.silence_card")
    } else {
        let (until, card) = silence::apply(nanty, request, session.channel);
        session.silence = Some(SilenceEffect::Applied(until));
        (until, card, "nanty.tool.silence_set")
    };
    keep_open(session);
    if !session.cards.contains(&card) {
        session.cards.push(card);
    }
    crate::i18n::tr(lang, code, &[("until", &hhmm(until))])
}

/// `project_status`: a fonte única da T02, em frases.
async fn project_status(state: &AppState, cfg: &Config, lang: Lang, secrets: &SecretSet, project: &Project) -> String {
    let head = line("nanty.status.project", &[("name", json!(project_name(secrets, project))), ("id", json!(project.id))]);
    let mut lines = vec![head];
    match super::observe::project_facts(state, cfg, &project.id).await {
        Some(facts) => lines.extend(status_lines(&facts, true)),
        None => lines.push(line("nanty.status.unread", &[])),
    }
    render_lines(lang, &lines)
}

/// Tira do corpo (JSON) qualquer `<` e `>`, trocados pelos escapes `\u003c` e `\u003e` do próprio
/// JSON (o valor lido não muda): nada do projeto abre nem fecha o envelope, em qualquer caixa.
pub fn seal(json: &str) -> String {
    json.replace('<', "\\u003c").replace('>', "\\u003e")
}

/// `project_details`: a leitura a pedido (snapshot e relatório, já redigidos), no envelope.
async fn project_details(state: &AppState, cfg: &Config, lang: Lang, project: &Project) -> String {
    let view = super::read::project_view(state, cfg, project).await;
    let body = seal(&serde_json::to_string(&view).unwrap_or_default());
    let intro = crate::i18n::tr(lang, "nanty.tool.details_intro", &[("id", project.id.as_str())]);
    format!("{intro}\n{UNTRUSTED_OPEN}\n{body}\n{UNTRUSTED_CLOSE}")
}

/// O silêncio de tudo ainda valendo agora (o mais longo).
pub fn global_silence(nanty: &Nanty, now_ms: u64) -> Option<u64> {
    nanty
        .snapshot()
        .silences
        .iter()
        .filter(|s| s.project_id.is_none() && s.group.is_none() && s.until_ms > now_ms)
        .map(|s| s.until_ms)
        .max()
}

/// A linha do orçamento do dia (ou a de sem modelo), como no `status`.
pub fn budget_line(nanty: &Nanty, cfg: &Config) -> Line {
    if turn::model_route(cfg).is_none() {
        return line("nanty.chat.status_no_model", &[]);
    }
    let round = |v: f64| (v * 100.0).round() / 100.0;
    let view = turn::budget_view(cfg, &nanty.snapshot().spend_on(&nanty.now().day_key()));
    // Spec 139 T06: nada gasto ainda não vira "0 de N".
    match view.mode {
        turn::BudgetMode::Usd if round(view.spent_usd) <= 0.0 => {
            line("nanty.chat.status_budget_unused_usd", &[("budget", json!(round(view.budget_usd)))])
        }
        turn::BudgetMode::Usd => {
            line("nanty.chat.status_budget_usd", &[("spent", json!(round(view.spent_usd))), ("budget", json!(round(view.budget_usd)))])
        }
        turn::BudgetMode::Tokens if view.tokens == 0 => {
            line("nanty.chat.status_budget_unused_tokens", &[("budget", json!(view.budget_tokens))])
        }
        turn::BudgetMode::Tokens => {
            line("nanty.chat.status_budget_tokens", &[("tokens", json!(view.tokens)), ("budget", json!(view.budget_tokens))])
        }
    }
}

/// `system_status`: o espaço livre (pela sonda), os órfãos do Docker (pelo que o observador viu), o
/// orçamento e o silêncio.
async fn system_status(nanty: &Nanty, cfg: &Config, lang: Lang) -> String {
    let probe = nanty.probe();
    let paths = crate::server::doctor::disk_paths(cfg);
    let measured = tokio::task::spawn_blocking(move || {
        let disk = paths.iter().filter_map(|p| probe.free_bytes(p)).min();
        let tmp = probe.free_bytes(&std::env::temp_dir());
        (disk, tmp)
    })
    .await
    .unwrap_or((None, None));
    let filter = &cfg.nanty.filter;
    let mut lines = Vec::new();
    for (bytes, code, min_gb) in
        [(measured.0, "nanty.system.disk", filter.disk_min_free_gb), (measured.1, "nanty.system.tmp", filter.tmp_min_free_gb)]
    {
        if let Some(bytes) = bytes {
            lines.push(line(code, &[("gb", json!(bytes / GB_BYTES))]));
            if bytes < min_gb.saturating_mul(GB_BYTES) {
                lines.push(line("nanty.system.below_min", &[("min", json!(min_gb))]));
            }
        }
    }
    if measured == (None, None) {
        lines.push(line("nanty.system.unmeasured", &[]));
    }
    let snap = nanty.snapshot();
    for (kind, code) in [(SignalKind::DockerOrphans, "nanty.system.orphans"), (SignalKind::DockerForeign, "nanty.system.foreign")] {
        let count: u64 = snap
            .alerts
            .values()
            .filter(|a| a.kind == kind.as_str())
            .filter_map(|a| a.facts.get("count").and_then(Value::as_u64))
            .sum();
        if count > 0 {
            lines.push(line(code, &[("count", json!(count))]));
        }
    }
    lines.push(budget_line(nanty, cfg));
    let now = nanty.now();
    if let Some(until) = global_silence(nanty, now.epoch_ms) {
        lines.push(line("nanty.chat.status_silence", &[("until", json!(hhmm(until)))]));
    }
    let n = &cfg.nanty;
    let (start, end) = n.quiet_window();
    if start != end {
        lines.push(line("nanty.system.quiet", &[("start", json!(n.quiet_start)), ("end", json!(n.quiet_end))]));
        if now.within((start, end)) {
            lines.push(line("nanty.system.quiet_now", &[]));
        }
    }
    render_lines(lang, &lines)
}

/// O nome (redigido) de um projeto observado; `None` para excluído ou desconhecido.
fn name_of(cfg: &Config, secrets: &SecretSet, id: &str) -> Option<String> {
    chat::observed(cfg).into_iter().find(|p| p.id == id).map(|p| project_name(secrets, p))
}

/// `nanty_activity`: o que foi feito e decidido hoje, o previsto e os cartões abertos.
fn activity(nanty: &Nanty, cfg: &Config, lang: Lang, secrets: &SecretSet) -> String {
    let now = nanty.now();
    let start = digest::day_start_ms(&now);
    let snap = nanty.snapshot();
    let mut lines: Vec<Line> = Vec::new();
    // Nada de projeto excluído: nem o nome, nem as ações, nem os cartões.
    let observed = |id: &Option<String>| id.as_deref().is_none_or(|id| name_of(cfg, secrets, id).is_some());
    let done: Vec<_> = snap.done.iter().filter(|d| d.at_ms >= start && observed(&d.project_id)).collect();
    for d in done.iter().rev().take(MAX_ACTIVITY).rev() {
        let action = render::code_text(lang, &d.code, &d.params);
        let failed = d.result == "failed";
        let mut params = vec![("at", json!(hhmm(d.at_ms))), ("action", json!(action))];
        let code = match &d.project_id {
            Some(id) => {
                params.push(("project", json!(name_of(cfg, secrets, id).unwrap_or_default())));
                if failed { "nanty.activity.failed" } else { "nanty.activity.done" }
            }
            None if failed => "nanty.activity.failed_global",
            None => "nanty.activity.done_global",
        };
        lines.push(line(code, &params));
    }
    let decided: Vec<_> = snap.decided.iter().filter(|d| d.at_ms >= start && observed(&d.project_id)).collect();
    for d in decided.iter().rev().take(MAX_ACTIVITY).rev() {
        let code = match d.decided_by {
            DecidedBy::Auto => "nanty.activity.decided.auto",
            DecidedBy::Web => "nanty.activity.decided.web",
            DecidedBy::Telegram => "nanty.activity.decided.telegram",
        };
        lines.push(line(code, &[("at", json!(hhmm(d.at_ms))), ("card", json!(d.card_id))]));
    }
    for p in snap.planned.iter().filter(|p| observed(&p.project_id)).take(MAX_ACTIVITY) {
        let action = render::code_text(lang, &p.code, &p.params);
        lines.push(line("nanty.activity.planned", &[("at", json!(hhmm(p.at_ms))), ("action", json!(action))]));
    }
    let open = snap
        .cards
        .iter()
        .filter(|c| {
            c.expires_at_ms > now.epoch_ms
                && c.urgency != Some(crate::config::Urgency::Digest)
                && c.awaits_person()
                && observed(&c.project_id)
        })
        .count();
    if lines.is_empty() && open == 0 {
        lines.push(line("nanty.activity.none", &[]));
    }
    if open > 0 {
        lines.push(line("nanty.activity.open_cards", &[("count", json!(open))]));
    }
    render_lines(lang, &lines)
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
