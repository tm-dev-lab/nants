//! O cenário proativo do Nanty (spec 140 T12, G6, C-11, C-12, C-14 e C-19).
//!
//! Nos avisos importantes, o Nanty lê o projeto do aviso por conta própria e manda à pessoa, numa
//! mensagem à parte, o que houve, a causa provável (sempre como hipótese) e uma sugestão. É a revisão
//! da C-01 da 138 pedida pelo usuário: conteúdo de projeto lido sem pedido, só nesses avisos.
//!
//! - **Interruptor:** `[nanty.analysis] proactive`, desligado por padrão. A aba Ajustes diz para onde
//!   o texto vai (o provedor da conexão do Nanty e o Telegram) antes de a pessoa ligar (C-12).
//! - **Gatilhos (C-14), como estado:** a cada passagem do observador ([`tick`]) os cartões abertos são
//!   olhados de novo. Execução bloqueada e laço de revisão contam só depois do cartão enviado (no
//!   Telegram, com mensagem própria; sem Telegram, na janela, depois da acomodação), se ele não foi
//!   decidido nem resolvido. O fim de spec vai sem ferramentas, só com o `report.md`, lido pelo leitor
//!   seguro ([`super::files`]), redigido e selado. Com o lugar ocupado (`busy`), o caso fica para a
//!   próxima passagem; ele só é marcado como analisado no `Ok` do [`analysis::spawn`].
//! - **Não roda** no horário de silêncio (nem no silêncio de tudo), em aviso que vai ao resumo, nem
//!   em aviso que saiu na rajada da fila do Telegram; e o cenário fica fora da rajada
//!   ([`super::telegram::outbox`]).
//! - **Sem repetir:** a chave `projeto:spec:tarefa:sinal:impressão dos fatos`, persistida no estado
//!   ([`super::store::ScenarioMark`], com a limpeza de projeto pelo `Scoped`), e os tetos por dia e
//!   por spec, mais a fatia do orçamento do dia ([`analysis::share_left`]).
//! - **Contexto mínimo (C-11):** só o sinal e o projeto do aviso (o nome, os fatos estruturados, o
//!   estado e, no fim de spec, o relatório). Sem memória, sem histórico, sem lista de outros projetos.
//!   A sessão nasce contaminada e presa a esse projeto ([`analysis::spawn`]).
//! - **Saída:** texto puro, sem campo de ações, até `[nanty.analysis] scenario_chars`, pelo redator e
//!   pelo `clean`. Antes disso, a guarda ([`guard`]) descarta o cenário inteiro se ele copia um
//!   trecho do que foi lido ([`MIN_LITERAL_WORDS`] palavras seguidas), se tem um caminho ou um
//!   trecho com forma de comando (o nome de uma ferramenta em prosa passa); no lugar, a pessoa recebe a
//!   linha fixa com a frase [`WITHHELD`]. Desligado o interruptor no meio, nada sai. A mensagem é separada do aviso, sem botões, abre com a linha fixa do servidor
//!   ([`LEAD`]) e entra na conversa com a procedência contaminada e o `project_ids` do aviso.
//! - **Voz:** o cenário vai só em texto, mesmo com a voz ligada. O aviso já falou (um áudio por
//!   aviso), e o cenário é material de leitura, com hipótese e sugestão: falado, ele custaria uma
//!   segunda fala longa logo depois da primeira, sem ganho para quem decide pelos botões.
//! - **O aviso nunca espera:** o cenário só começa depois do cartão enviado, numa tarefa à parte, e a
//!   mensagem sai depois, pela mesma fila.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{json, Map, Value};

use super::agent::{Ended, Session};
use super::analysis::{self, Cancel, Done, Job, Origin};
use super::files::{self, LineRange};
use super::journal::{JournalEntry, JournalKind};
use super::provenance::TaintKind;
use super::signals::SignalKind;
use super::store::{Card, Channel, ConversationMessage, Delivery, NantyState, ScenarioMark};
use super::telegram::format;
use super::{nanty_lang, new_id, tools, turn, Nanty};
use crate::agents::llm_client::{ChatMessage, Role};
use crate::config::{Config, Urgency};
use crate::i18n::Lang;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// A linha fixa do servidor que abre o cenário: é uma análise automática, e a causa é hipótese.
pub const LEAD: &str = "nanty.scenario.lead";
/// A frase do servidor no lugar do cenário que a guarda recusou: houve análise, e a pessoa pode pedir
/// na conversa.
pub const WITHHELD: &str = "nanty.scenario.withheld";
/// Palavras seguidas iguais às de um trecho lido que fazem do cenário uma cópia (a guarda).
pub const MIN_LITERAL_WORDS: usize = 8;
/// Fatos que mudam a cada renovação do cartão: ficam fora da impressão do caso.
const VOLATILE: [&str; 4] = ["since", "age_min", "observer", "free_gb"];
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---- O estado em memória ----

/// O cenário em andamento (o cancelamento dele) e as recusas já anotadas no registro, para uma
/// condição que dura não encher o registro a cada passagem.
#[derive(Default)]
pub struct Runtime {
    current: Mutex<Option<Cancel>>,
    noted: Mutex<BTreeSet<String>>,
    /// Relatórios lidos para o fim de spec (a conferência de que uma recusa não refaz a leitura).
    reports_read: std::sync::atomic::AtomicUsize,
}

impl Runtime {
    /// O interruptor foi desligado: o cenário em andamento para na hora, até a espera do modelo.
    pub fn stop(&self) {
        if let Some(cancel) = self.current() {
            cancel.cancel();
        }
    }

    /// Relatórios lidos até agora.
    pub fn reports_read(&self) -> usize {
        self.reports_read.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set(&self, cancel: Option<Cancel>) {
        *lock(&self.current) = cancel;
    }

    fn current(&self) -> Option<Cancel> {
        lock(&self.current).clone()
    }

    /// `true` na primeira vez desta anotação.
    fn note_once(&self, what: String) -> bool {
        let mut noted = lock(&self.noted);
        if noted.len() > 1000 {
            noted.clear();
        }
        noted.insert(what)
    }
}

// ---- Os gatilhos ----

/// O sinal do cartão, se é um dos gatilhos: execução bloqueada, laço de revisão e fim de spec.
pub fn trigger(card: &Card) -> Option<SignalKind> {
    SignalKind::of_why_code(&card.code).filter(|k| matches!(k, SignalKind::RunBlocked | SignalKind::ReviewLoop | SignalKind::SpecDone))
}

fn param_text<'a>(card: &'a Card, key: &str) -> Option<&'a str> {
    card.params.get(key).and_then(Value::as_str).map(str::trim).filter(|v| !v.is_empty())
}

/// A impressão dos fatos do aviso: os fatos estáveis, em ordem de chave, num hash curto.
fn fingerprint(params: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = params.keys().filter(|k| !VOLATILE.contains(&k.as_str())).collect();
    keys.sort();
    let stable: Vec<String> = keys.iter().map(|k| format!("{k}={}", params.get(*k).map(Value::to_string).unwrap_or_default())).collect();
    let hash = crate::server::devices::hash_hex(&stable.join("\n"));
    hash.chars().take(16).collect()
}

/// A chave do caso, `projeto:spec:tarefa:sinal:impressão dos fatos`, e a spec do aviso.
pub fn case_key(card: &Card, kind: SignalKind) -> Option<(String, Option<String>)> {
    let project = card.project_id.as_deref()?;
    let spec = param_text(card, "spec_id").map(str::to_string);
    let task = param_text(card, "task_id").unwrap_or("-");
    let key = format!("{project}:{}:{task}:{}:{}", spec.as_deref().unwrap_or("-"), kind.as_str(), fingerprint(&card.params));
    Some((key, spec))
}

/// O canal do Telegram está de pé na configuração: dono pareado e token com a forma certa.
fn telegram_on(cfg: &Config) -> bool {
    let tg = &cfg.nanty.telegram;
    tg.is_paired() && tg.resolve_token().is_some_and(|t| super::telegram::valid_token(&t))
}

/// Como o cartão está na saída.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Out {
    /// Já chegou à pessoa, com mensagem própria.
    Sent,
    /// Ainda vai sair: o caso espera a próxima passagem.
    Waiting,
    /// Saiu junto, no resumo da fila (a rajada), ou o Telegram o recusou: sem cenário.
    Burst,
}

/// O cartão já chegou à pessoa? Com o Telegram, pela mensagem própria dele (a da renovação, se houve);
/// sem ele, na janela, passada a acomodação.
fn card_out(st: &NantyState, telegram: bool, card: &Card, now_ms: u64, settle_ms: u64) -> Out {
    if !telegram {
        return if now_ms >= card.created_at_ms.saturating_add(settle_ms) { Out::Sent } else { Out::Waiting };
    }
    match st.telegram_sent.iter().find(|r| r.card_id == card.id && r.created_at_ms >= card.created_at_ms) {
        None => Out::Waiting,
        Some(r) if r.message_id.is_some() => Out::Sent,
        Some(_) => Out::Burst,
    }
}

/// O cartão pode ter cenário agora: gatilho, de projeto observado, aberto, aviso de observador, que
/// não vai ao resumo.
fn eligible(cfg: &Config, card: &Card, now_ms: u64) -> Option<SignalKind> {
    let kind = trigger(card)?;
    let project = card.project_id.as_deref()?;
    let digest = card.delivery == Some(Delivery::Digest) || card.urgency == Some(Urgency::Digest);
    (cfg.nanty.observes(project) && cfg.projects.iter().any(|p| p.id == project) && card.expires_at_ms > now_ms && card.requested.is_none() && !digest)
        .then_some(kind)
}

fn entry(nanty: &Nanty, code: &str, project_id: Option<&str>, params: &[(&str, Value)]) -> JournalEntry {
    let mut e = JournalEntry::new(nanty.now().epoch_ms, JournalKind::ModelTurn, code);
    e.project_id = project_id.map(str::to_string);
    e.params.insert("turn".into(), json!("scenario"));
    for (k, v) in params {
        e.params.insert((*k).to_string(), v.clone());
    }
    e
}

/// Uma recusa que dura (teto, sem relatório, sem orçamento, sem fatia, falha do cliente): no registro
/// uma vez por caso, motivo e dia.
fn note(nanty: &Nanty, key: &str, project: &str, reason: &'static str) {
    if nanty.scenario().note_once(format!("{key}|{reason}|{}", nanty.now().day_key())) {
        nanty.record(&entry(nanty, "scenario.skipped", Some(project), &[("reason", json!(reason))]));
    }
}

/// Uma passagem do observador: confere o cenário em andamento (cancela o que se resolveu) e, se o
/// lugar da análise está livre, começa o primeiro caso pendente. Devolve a tarefa que entrega o
/// cenário, quando um começou (os testes esperam por ela; o laço a solta). Nunca espera o modelo.
pub async fn tick(state: &AppState, nanty: &Arc<Nanty>) -> Option<tokio::task::JoinHandle<()>> {
    let cfg: Config = state.config().read().await.clone();
    // Um cenário em andamento: a passagem só confere se o caso dele se resolveu (ou se o interruptor
    // foi desligado) e nunca começa outro junto.
    if let Some(cancel) = nanty.scenario().current() {
        if cancel.is_cancelled() || !cfg.nanty.analysis.proactive {
            cancel.cancel();
        }
        return None;
    }
    if !cfg.nanty.analysis.proactive || turn::model_route(&cfg).is_none() || nanty.analysis().running() {
        return None;
    }
    let now = nanty.now();
    let st = nanty.snapshot();
    // Horário de silêncio e silêncio de tudo: não roda agora (o caso volta depois, se seguir aberto).
    if now.within(cfg.nanty.quiet_window()) || super::silence::global_until(&st, now.epoch_ms).is_some() {
        return None;
    }
    let telegram = telegram_on(&cfg);
    let settle_ms = u64::try_from(nanty.telegram().tuning().send_settle.as_millis()).unwrap_or(u64::MAX);
    let a = &cfg.nanty.analysis;
    let today = now.day_key();
    let mut cards: Vec<&Card> = st.cards.iter().collect();
    cards.sort_by_key(|c| c.created_at_ms);
    for card in cards {
        let Some(kind) = eligible(&cfg, card, now.epoch_ms) else { continue };
        let Some((key, spec)) = case_key(card, kind) else { continue };
        let Some(project) = card.project_id.clone() else { continue };
        if st.scenarios.iter().any(|m| m.key == key) {
            continue;
        }
        match card_out(&st, telegram, card, now.epoch_ms, settle_ms) {
            Out::Waiting => continue,
            Out::Burst => {
                note(nanty, &key, &project, "burst");
                continue;
            }
            Out::Sent => {}
        }
        if st.scenarios.iter().filter(|m| m.day == today).count() >= a.per_day() {
            note(nanty, &key, &project, "daily_cap");
            continue;
        }
        if st.scenarios.iter().filter(|m| m.project_id == project && m.spec_id == spec).count() >= a.per_spec() {
            note(nanty, &key, &project, "spec_cap");
            continue;
        }
        // Sem orçamento ou sem fatia no dia: a recusa fica no registro uma vez, sem montar a análise
        // (nem ler o relatório) a cada passagem. No dia seguinte, o caso aberto volta a ser tentado.
        let spend = st.spend_on(&today);
        if turn::budget_view(&cfg, &spend).exhausted {
            note(nanty, &key, &project, "budget");
            continue;
        }
        if !analysis::share_left(&cfg, &spend) {
            note(nanty, &key, &project, "share");
            continue;
        }
        // Sem cliente para a conexão do Nanty: uma linha só, sem montar a análise (revisão 2).
        let Some((conn, _)) = turn::model_route(&cfg) else { return None };
        if super::agent::backend(state, nanty, conn).is_none() {
            note(nanty, &key, &project, "failed");
            continue;
        }
        let Some(job) = job(state, nanty, &cfg, card, kind).await else {
            note(nanty, &key, &project, "no_report");
            continue;
        };
        let cancel = job.cancel.clone();
        let handle = match analysis::spawn(state, nanty, &cfg, job) {
            Ok(handle) => handle,
            // O caso já se resolveu: o próximo pode seguir.
            Err("cancelled") => continue,
            // Recusas que duram: uma linha por caso e motivo.
            Err(reason @ ("budget" | "share" | "failed")) => {
                note(nanty, &key, &project, reason);
                return None;
            }
            // Ocupado ou sem modelo: fica para a próxima passagem.
            Err(_) => return None,
        };
        // Só agora o caso conta como analisado (C-14).
        let mark = ScenarioMark { key, project_id: project.clone(), spec_id: spec, day: today.clone(), at_ms: now.epoch_ms };
        nanty.update(|st| {
            st.push_scenario(mark);
            true
        });
        nanty.scenario().set(Some(cancel));
        let (state, n, card, lang) = (state.clone(), Arc::clone(nanty), card.clone(), nanty_lang(&cfg.nanty, nanty));
        return Some(tokio::spawn(async move {
            let done = handle.await;
            n.scenario().set(None);
            match done {
                Ok(done) => deliver(&state, &n, lang, &card, kind, done).await,
                Err(_) => n.record(&entry(&n, "scenario.dropped", Some(&project), &[("reason", json!("failed"))])),
            }
        }));
    }
    None
}

// ---- A entrada ----

/// O relatório da spec no envelope de dado não confiável, pelo leitor seguro (redigido e selado).
fn report(cfg: &Config, secrets: &SecretSet, project: &str, spec: &str) -> Option<String> {
    if !(turn::is_code(spec) && crate::server::spec_result::validate_spec_id(spec).is_ok()) {
        return None;
    }
    let root = files::Root::for_project(cfg, project).ok()?;
    let mut budget = files::Budget::new(&cfg.nanty.read);
    let file = root.read(&format!(".sdd/specs/{spec}/report.md"), LineRange::default(), &cfg.nanty.read, secrets, &mut budget).ok()?;
    (!file.text.trim().is_empty()).then(|| file.untrusted())
}

/// A análise do caso: o prompt, a entrada com só o sinal e o projeto do aviso, a sessão de análise e
/// o cancelamento pelo cartão. O fim de spec vai com o relatório e sem ferramentas (uma rodada só);
/// sem relatório, nada (`None`).
async fn job(state: &AppState, nanty: &Arc<Nanty>, cfg: &Config, card: &Card, kind: SignalKind) -> Option<Job> {
    let project_id = card.project_id.clone()?;
    let project = cfg.projects.iter().find(|p| p.id == project_id)?;
    let lang = nanty_lang(&cfg.nanty, nanty);
    let secrets = SecretSet::from_config(cfg);
    let facts = super::observe::project_facts(state, cfg, &project_id).await;
    // Spec 141 T09 (C-09): o estado em dados, com o sinal de falha do Nants quando houver.
    let status = facts.as_ref().map(|f| Value::Object(tools::status_json(f))).unwrap_or(Value::Null);
    let mut body = json!({
        "signal": kind.as_str(),
        "project": { "id": project_id, "name": tools::project_name(&secrets, project) },
        "facts": turn::structured(&card.params),
        "status": status,
    });
    let mut session = Session::analysis(Channel::Web, super::provenance::Taint::default(), lang);
    let tools_on = kind != SignalKind::SpecDone;
    if !tools_on {
        let spec = param_text(card, "spec_id")?.to_string();
        let (c, s, p) = (cfg.clone(), secrets.clone(), project_id.clone());
        nanty.scenario().reports_read.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let report = tokio::task::spawn_blocking(move || report(&c, &s, &p, &spec)).await.ok().flatten()?;
        body["report"] = json!(report);
        // O relatório entra na conferência da saída, como o que as ferramentas leem.
        session.seen.push(report);
    }
    let system = crate::agents::prompts::nanty_scenario_system(lang, cfg.nanty.analysis.max_chars(), tools_on);
    let context = vec![ChatMessage::text(Role::User, secrets.scrub_exact(&body.to_string()))];
    let weak = Arc::downgrade(nanty);
    let card_id = card.id.clone();
    let config = state.config_shared();
    // Decidido, resolvido ou vencido, ou o interruptor desligado: a análise para (C-14). A
    // configuração é lida sem esperar; ocupada numa gravação, vale a próxima conferência.
    let cancel = Cancel::when(move || {
        weak.upgrade().is_none_or(|n| !n.card_open(&card_id)) || config.try_read().is_ok_and(|c| !c.nanty.analysis.proactive)
    });
    let origin = Origin { project_id, taint: TaintKind::ProjectFile };
    let mut job = Job::new(cfg, origin, system, context, session, cancel);
    if !tools_on {
        // Uma rodada: a última vai sem ferramentas.
        job.limits.rounds = 1;
    }
    Some(job)
}

// ---- A saída ----

/// Entrega o cenário: o texto do modelo passa pela guarda, pelo `clean` e pelo teto, e vira a
/// mensagem à parte, contaminada e ligada ao projeto do aviso. O que não passa fica só no registro.
async fn deliver(state: &AppState, nanty: &Arc<Nanty>, lang: Lang, card: &Card, kind: SignalKind, done: Done) {
    let project = card.project_id.clone().unwrap_or_default();
    let dropped = |reason: &str| nanty.record(&entry(nanty, "scenario.dropped", Some(&project), &[("reason", json!(reason)), ("signal", json!(kind.as_str()))]));
    let text = match &done.ended {
        Ended::Reply { text, .. } => text.clone(),
        Ended::Skipped(reason) => return dropped(reason),
    };
    // O caso se resolveu enquanto o modelo escrevia: o cenário não vale mais.
    if !nanty.card_open(&card.id) {
        return dropped("resolved");
    }
    let cfg = state.config().read().await.clone();
    // Desligado no meio da análise: nada chega à pessoa (revisão 1, F-02).
    if !cfg.nanty.analysis.proactive {
        return dropped("off");
    }
    if let Err(reason) = guard(&text, &done.session.seen) {
        // A pessoa fica sabendo que houve análise: a linha fixa e a frase do servidor, sem o texto.
        dropped(reason);
        let mut message = ConversationMessage::from_nanty(new_id(), nanty.now().epoch_ms, Channel::Web, LEAD, Map::new());
        // Uma linha extra do dicionário, `{code, params}`, como as do resumo.
        let mut line = Map::new();
        line.insert("code".into(), json!(WITHHELD));
        line.insert("params".into(), json!({}));
        message.params.insert("lines".into(), json!([line]));
        message.lang = Some(lang.tag().to_string());
        message.link_projects(std::slice::from_ref(&project));
        let n = Arc::clone(nanty);
        let _ = tokio::task::spawn_blocking(move || n.push_message(&message)).await;
        return;
    }
    let secrets = SecretSet::from_config(&cfg);
    let clean = format::clean(&secrets, &text);
    let text = format::fit(&clean, cfg.nanty.analysis.max_chars());
    if text.trim().is_empty() {
        return dropped("empty");
    }
    let mut message = ConversationMessage::from_nanty(new_id(), nanty.now().epoch_ms, Channel::Web, LEAD, Map::new());
    message.text = Some(text);
    message.lang = Some(lang.tag().to_string());
    message.link_projects(std::slice::from_ref(&project));
    // Spec 140 T04 (C-01): a procedência do turno, que nasceu contaminada pelo projeto do aviso.
    message.taint = Some(done.session.tainted.clone());
    let n = Arc::clone(nanty);
    let _ = tokio::task::spawn_blocking(move || n.push_message(&message)).await;
    nanty.record(&entry(nanty, "scenario.sent", Some(&project), &[("signal", json!(kind.as_str()))]));
    super::summarize::after_reply(state, nanty).await;
}

// ---- A guarda ----

/// Os textos de dentro dos envelopes de dado não confiável: as folhas de texto do JSON selado, ou o
/// corpo cru se ele não for JSON.
fn untrusted_texts(seen: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for item in seen {
        let mut rest = item.as_str();
        while let Some(start) = rest.find(tools::UNTRUSTED_OPEN) {
            let after = &rest[start + tools::UNTRUSTED_OPEN.len()..];
            let Some(end) = after.find(tools::UNTRUSTED_CLOSE) else { break };
            let body = after[..end].trim();
            match serde_json::from_str::<Value>(body) {
                Ok(value) => leaves(&value, &mut out),
                Err(_) => out.push(body.to_string()),
            }
            rest = &after[end + tools::UNTRUSTED_CLOSE.len()..];
        }
    }
    out
}

fn leaves(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => items.iter().for_each(|v| leaves(v, out)),
        Value::Object(map) => map.values().for_each(|v| leaves(v, out)),
        _ => {}
    }
}

/// As palavras de um texto, em minúsculas, só letras e dígitos.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// O cenário copia um trecho lido: [`MIN_LITERAL_WORDS`] palavras seguidas iguais às de um texto que
/// veio do projeto (a definição objetiva de "trecho literal").
fn copies(text: &str, seen: &[String]) -> bool {
    let mut grams: BTreeSet<String> = BTreeSet::new();
    for source in untrusted_texts(seen) {
        let w = words(&source);
        for window in w.windows(MIN_LITERAL_WORDS) {
            grams.insert(window.join(" "));
        }
    }
    words(text).windows(MIN_LITERAL_WORDS).any(|window| grams.contains(&window.join(" ")))
}

/// Uma palavra com cara de caminho: começa na raiz, na pasta pessoal ou na atual, tem barra invertida,
/// ou tem barras ligando três partes, ou duas em que uma tem um ponto de arquivo.
fn is_path(word: &str) -> bool {
    let w = word.trim_matches(|c: char| matches!(c, '(' | ')' | '[' | ']' | '"' | '\'' | ',' | ';' | ':' | '!' | '?'));
    let w = w.trim_end_matches('.');
    if w.len() < 2 {
        return false;
    }
    let bytes = w.as_bytes();
    if bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        return true;
    }
    if w.starts_with("~/") || w.starts_with("./") || w.starts_with("../") || w.contains('\\') {
        return true;
    }
    if w.starts_with('/') {
        return w[1..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_');
    }
    let parts: Vec<&str> = w.split('/').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    let filelike = |p: &&str| p.find('.').is_some_and(|i| i > 0 && i + 1 < p.len());
    parts.len() >= 3 || parts.iter().any(filelike)
}

/// Uma palavra com cara de nome de executável (minúsculas, dígitos e `._-`).
fn executable(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}

/// Uma opção de linha de comando: `-x` ou `--xyz`.
fn option(word: &str) -> bool {
    let rest = word.strip_prefix("--").or_else(|| word.strip_prefix('-'));
    rest.is_some_and(|r| r.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && r.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Quem baixa da rede.
const DOWNLOADERS: [&str; 4] = ["curl", "wget", "iwr", "invoke-webrequest"];
/// Gerenciadores de pacote: com um verbo de instalação ([`INSTALL_VERBS`]) e um alvo, instalam.
const PACKAGE_MANAGERS: [&str; 16] =
    ["npm", "pnpm", "yarn", "bun", "pip", "pip3", "pipx", "uv", "cargo", "gem", "go", "apt", "apt-get", "brew", "composer", "dotnet"];
const INSTALL_VERBS: [&str; 4] = ["install", "i", "add", "get"];
/// Quem roda um pacote baixado na hora (o `pnpm dlx` é conferido à parte).
const RUNNERS: [&str; 3] = ["npx", "uvx", "bunx"];
/// Palavras que, logo depois do comando, mostram que a frase fala dele, em vez de dar um alvo: os
/// verbos do que aconteceu e as palavras curtas da prosa, nos dois idiomas, escritas à mão.
const NOT_A_TARGET: [&str; 58] = [
    "falhou", "quebrou", "travou", "parou", "deu", "demorou", "passou", "rodou", "terminou", "ficou", "voltou", "foi", "era", "está", "esta",
    "não", "nao", "etapa", "passo", "comando", "erro", "saída", "saida", "de", "do", "da", "no", "na", "em", "e", "ou", "que", "com", "para",
    "mas", "também", "failed", "broke", "hung", "stopped", "crashed", "timed", "passed", "ran", "was", "is", "did", "step", "command",
    "error", "output", "and", "or", "of", "in", "on", "for", "but",
];

/// A palavra seguinte é um alvo: existe, não termina a oração (a palavra do comando não fecha com
/// pontuação) e não está em [`NOT_A_TARGET`].
fn has_target(tokens: &[String], raw: &[&str], i: usize) -> bool {
    let closes = raw.get(i).is_some_and(|w| w.ends_with([',', '.', ';', ':', '!', '?', ')']));
    !closes && tokens.get(i + 1).is_some_and(|next| !next.is_empty() && !NOT_A_TARGET.contains(&next.as_str()))
}

/// O texto tem um trecho com forma de comando, pronto para copiar e colar: crase, linha que começa
/// com um prompt (`$`, `#`, `>`, `%`), encadeamento (`|`, `&&`, `||`, `;` colado) ou redirecionamento,
/// opção junto de um executável, `sudo`, um download, uma instalação ou um pacote rodado na hora, os
/// três com um alvo ([`has_target`]). O nome de uma ferramenta em prosa ("o npm install falhou")
/// passa.
fn has_command(text: &str) -> bool {
    if text.contains('`') {
        return true;
    }
    let prompt = |l: &str| ["$ ", "# ", "> ", "% "].iter().any(|p| l.trim_start().starts_with(p));
    if text.lines().any(prompt) {
        return true;
    }
    if ["&&", "||", "$(", ">>", "2>", "|sh", "| sh", "| bash"].iter().any(|op| text.contains(op)) {
        return true;
    }
    let trim = |w: &str| w.trim_matches(|c: char| matches!(c, ',' | '.' | ':' | '(' | ')' | '"' | '\'')).to_string();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    for (i, raw) in tokens.iter().enumerate() {
        let t = trim(raw);
        // A barra vertical entre duas palavras é um encadeamento; o `;` colado também.
        if *raw == "|" && i > 0 && i + 1 < tokens.len() {
            return true;
        }
        if t.find(';').is_some_and(|at| at > 0 && at + 1 < t.len()) {
            return true;
        }
        if t.eq_ignore_ascii_case("sudo") && i + 1 < tokens.len() {
            return true;
        }
        let before = i.checked_sub(1).map(|j| trim(tokens[j]));
        if option(&t) && (t.starts_with("--") || before.as_deref().is_some_and(executable)) {
            return true;
        }
    }
    // Download, instalação e pacote rodado na hora, sempre com um alvo (revisão 2, F-04).
    let words: Vec<String> = tokens.iter().map(|w| trim(w).to_lowercase()).collect();
    for (i, w) in words.iter().enumerate() {
        let w = w.as_str();
        if (DOWNLOADERS.contains(&w) || RUNNERS.contains(&w)) && has_target(&words, &tokens, i) {
            return true;
        }
        let sub = words.get(i + 1).map(String::as_str);
        let installs = PACKAGE_MANAGERS.contains(&w) && sub.is_some_and(|v| INSTALL_VERBS.contains(&v));
        let dlx = w == "pnpm" && sub == Some("dlx");
        if (installs || dlx) && !tokens[i].ends_with([',', '.', ';', ':']) && has_target(&words, &tokens, i + 1) {
            return true;
        }
    }
    false
}

/// A guarda da saída (C-11): `Err` com o motivo quando o cenário copia um trecho lido (`literal`), tem
/// um caminho (`path`) ou um trecho com forma de comando (`command`). O cenário que não passa não sai:
/// nenhum pedaço dele chega à pessoa, nem cortado; no lugar, vai a frase [`WITHHELD`].
pub fn guard(text: &str, seen: &[String]) -> Result<(), &'static str> {
    if copies(text, seen) {
        return Err("literal");
    }
    if text.split_whitespace().any(is_path) {
        return Err("path");
    }
    if has_command(text) {
        return Err("command");
    }
    Ok(())
}

#[cfg(test)]
#[path = "scenario_tests.rs"]
mod tests;
