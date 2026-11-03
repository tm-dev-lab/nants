//! Testes do lugar da análise (spec 140 T11, C-13), só com o modelo simulado (nunca chave real). O
//! modelo da análise fica preso num semáforo, liberado pelo teste: nenhum teste conta com quanto algo
//! demora. O prazo é medido no relógio pausado do tokio.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::sync::{Notify, Semaphore};

use super::*;
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{ChatRequest, ChatResponse, Role};
use crate::agents::LlmError;
use crate::i18n::Lang;
use crate::server::nanty::agent::tests::{call, say, Scripted, Turn};
use crate::server::nanty::chat::tests::{base_config, write_project};
use crate::server::nanty::chat::{person_message, reply};
use crate::server::nanty::provenance::Taint;
use crate::server::nanty::store::{Card, Channel, ConversationMessage};
use crate::server::nanty::{Avatar, InstanceStatus};

/// O prompt de sistema das análises do teste: é por ele que o modelo simulado as reconhece.
const ANALYSIS_SYSTEM: &str = "analysis-under-test";

// ---- O modelo simulado com portões ----

/// Dois roteiros: o da análise, preso num semáforo (uma licença por rodada), e o dos outros turnos
/// (a conversa e o aviso), com um portão que só fecha quando o teste pede.
struct Gate {
    analysis: Arc<Scripted>,
    other: Arc<Scripted>,
    analysis_open: Semaphore,
    other_open: Semaphore,
    other_gated: bool,
    analysis_in: Notify,
    other_in: Notify,
    /// Pedidos da análise que chegaram ao portão (os presos contam).
    analysis_asked: AtomicUsize,
}

impl ChatBackend for Gate {
    fn complete<'a>(&'a self, req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            if req.messages.first().is_some_and(|m| m.content == ANALYSIS_SYSTEM) {
                self.analysis_asked.fetch_add(1, Ordering::SeqCst);
                self.analysis_in.notify_one();
                self.analysis_open.acquire().await.expect("portão da análise").forget();
                return self.analysis.complete(req).await;
            }
            self.other_in.notify_one();
            if self.other_gated {
                self.other_open.acquire().await.expect("portão da conversa").forget();
            }
            self.other.complete(req).await
        })
    }
}

// ---- Mundo de teste ----

struct World {
    dir: PathBuf,
    state: AppState,
    gate: Arc<Gate>,
}

impl World {
    /// O projeto `p1`, o roteiro da análise e o dos outros turnos (`other_gated`: a conversa também
    /// espera o teste).
    fn new(tag: &str, analysis: Vec<Turn>, other: Vec<Turn>, other_gated: bool, edit: impl FnOnce(&mut Config)) -> Self {
        let dir = std::env::temp_dir().join(format!("nants-nanty-140t11-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut cfg = base_config(&dir, &[("p1", "Loja")]);
        edit(&mut cfg);
        write_project(&dir.join("p1"), "Loja", "nada", None);
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(crate::server::nanty::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        let gate = Arc::new(Gate {
            analysis: Scripted::new(analysis),
            other: Scripted::new(other),
            analysis_open: Semaphore::new(0),
            other_open: Semaphore::new(0),
            other_gated,
            analysis_in: Notify::new(),
            other_in: Notify::new(),
            analysis_asked: AtomicUsize::new(0),
        });
        state.nanty().set_backend(Arc::clone(&gate) as Arc<dyn ChatBackend>);
        Self { dir, state, gate }
    }

    fn nanty(&self) -> Arc<Nanty> {
        Arc::clone(self.state.nanty())
    }

    async fn cfg(&self) -> Config {
        self.state.config().read().await.clone()
    }

    /// Uma análise do `p1` com a condição de cancelamento dada.
    async fn job(&self, cancel: Cancel) -> Job {
        let context = vec![ChatMessage::text(Role::User, json!({"signal": "run_blocked", "project_id": "p1"}).to_string())];
        let session = Session::analysis(Channel::Telegram, Taint::default(), Lang::PtBr);
        let origin = Origin { project_id: "p1".into(), taint: crate::server::nanty::provenance::TaintKind::ProjectFile };
        Job::new(&self.cfg().await, origin, ANALYSIS_SYSTEM.into(), context, session, cancel)
    }

    async fn spawn(&self, cancel: Cancel) -> Result<tokio::task::JoinHandle<Done>, &'static str> {
        let job = self.job(cancel).await;
        spawn(&self.state, &self.nanty(), &self.cfg().await, job)
    }

    /// A pessoa escreve na janela; devolve a resposta do Nanty.
    async fn say(&self, text: &str) -> ConversationMessage {
        say_in(&self.state, text).await
    }

    /// Os registros de turno da análise com o código.
    fn analysis_turns(&self, code: &str) -> Vec<Map<String, Value>> {
        self.nanty()
            .journal_tail(1000)
            .into_iter()
            .filter(|e| e.code == code && e.params.get("turn") == Some(&json!("analysis")))
            .map(|e| e.params)
            .collect()
    }

    fn avatar(&self) -> Avatar {
        self.nanty().avatar(&crate::config::NantyConfig::default())
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn say_in(state: &AppState, text: &str) -> ConversationMessage {
    let secrets = state.secret_set().await;
    let cfg = state.config().read().await.nanty.clone();
    let person = person_message(state.nanty(), &cfg, &secrets, Channel::Web, text).expect("mensagem");
    reply(state, &person).await.expect("resposta")
}

fn project_status() -> Turn {
    call("project_status", json!({"project_id": "p1"}))
}

fn reply_text(done: &Done) -> Option<&str> {
    match &done.ended {
        Ended::Reply { text, .. } => Some(text),
        Ended::Skipped(_) => None,
    }
}

// ---- Fora da vez da conversa ----

#[tokio::test]
async fn a_long_analysis_never_delays_the_conversation_nor_the_notice() {
    let notice = r#"{"summary": "Um plano espera.", "actions": []}"#;
    let w = World::new("apart", vec![say("O teste falhou; causa provável: dependência.")], vec![say("Oi! Tudo certo por aqui."), say(notice)], false, |_| {});
    let analysis = w.spawn(Cancel::new()).await.expect("começou");
    w.gate.analysis_in.notified().await;
    assert!(w.nanty().analysis().running());

    // A conversa responde com a análise presa no modelo.
    let answer = w.say("oi").await;
    assert!(answer.text.as_deref().is_some_and(|t| t.contains("Tudo certo")), "{answer:?}");

    // O aviso também: o turno proativo do cartão novo sai inteiro.
    let nanty = w.nanty();
    let now = nanty.now().epoch_ms;
    nanty.update(|st| {
        st.cards.push(Card {
            id: "c1".into(),
            project_id: Some("p1".into()),
            group: "p1:plan_waiting".into(),
            code: "nanty.why.plan_waiting".into(),
            created_at_ms: now,
            expires_at_ms: now + 3_600_000,
            ..Card::default()
        });
        true
    });
    let outcome = turn::proactive(&w.state, &nanty).await;
    assert!(matches!(outcome, Some(turn::TurnOutcome::Reply(_))), "{outcome:?}");

    // Nada disso passou pela análise, que segue presa, nem ela pela fila da conversa.
    assert_eq!(nanty.turns().queued(), 0, "a análise não ocupa a fila da conversa");
    assert!(nanty.analysis().running());
    assert_eq!(w.gate.analysis_asked.load(Ordering::SeqCst), 1);
    assert_eq!(w.gate.other.requests().len(), 2);

    w.gate.analysis_open.add_permits(1);
    let done = analysis.await.expect("tarefa");
    assert_eq!(reply_text(&done), Some("O teste falhou; causa provável: dependência."));
    assert!(!nanty.analysis().running(), "o lugar fica livre no fim");
    assert_eq!(w.analysis_turns("turn.ok").len(), 1);
    // Só as ferramentas da análise (spec 140 T04, C-06).
    let tools: Vec<String> = w.gate.analysis.requests()[0].tools.iter().map(|t| t.name.clone()).collect();
    assert_eq!(tools, tools::Mode::Analysis.tools().iter().map(|t| t.to_string()).collect::<Vec<_>>());
    // Spec 140 T12 (C-11): nasce contaminada pelo projeto do aviso e presa a ele.
    assert_eq!(done.session.pinned.as_deref(), Some("p1"));
    assert!(done.session.tainted.sources.iter().any(|s| s.project_id.as_deref() == Some("p1")));
}

/// Decisão (C-13 e G6): a segunda análise é descartada, não enfileirada. Na fila, sairia atrasada,
/// sobre um caso que pode já ter mudado; quem chama só marca o caso quando a análise começa.
#[tokio::test]
async fn a_second_analysis_is_dropped_while_one_runs() {
    let w = World::new("second", vec![say("primeira"), say("terceira")], Vec::new(), false, |_| {});
    let first_cancel = Cancel::new();
    let first = w.spawn(first_cancel.clone()).await.expect("primeira");
    w.gate.analysis_in.notified().await;
    assert_eq!(w.spawn(Cancel::new()).await.err(), Some("busy"));
    assert_eq!(w.analysis_turns("turn.skipped")[0]["reason"], "busy");
    assert_eq!(w.gate.analysis_asked.load(Ordering::SeqCst), 1, "a segunda nem chegou ao modelo");

    first_cancel.cancel();
    assert!(matches!(first.await.expect("tarefa").ended, Ended::Skipped("cancelled")));
    // Com o lugar livre, a próxima roda.
    let third = w.spawn(Cancel::new()).await.expect("terceira");
    w.gate.analysis_open.add_permits(1);
    assert_eq!(reply_text(&third.await.expect("tarefa")), Some("primeira"));
}

// ---- Os tetos ----

/// O prazo padrão (90 s) no relógio pausado: o modelo nunca responde, e só o prazo encerra.
#[tokio::test(start_paused = true)]
async fn the_deadline_ends_the_analysis() {
    let w = World::new("deadline", vec![say("tarde demais")], Vec::new(), false, |_| {});
    assert_eq!(Limits::analysis(&w.cfg().await).deadline, Duration::from_secs(90));
    let started = tokio::time::Instant::now();
    let done = w.spawn(Cancel::new()).await.expect("começou").await.expect("tarefa");
    assert!(matches!(done.ended, Ended::Skipped("deadline")), "{:?}", done.ended);
    assert!(started.elapsed() >= Duration::from_secs(90));
    assert_eq!(w.analysis_turns("turn.skipped")[0]["reason"], "deadline");
    assert!(!w.nanty().analysis().running());
    assert_ne!(w.avatar(), Avatar::Thinking);
}

#[tokio::test]
async fn the_round_and_token_caps_end_the_analysis() {
    // Quatro rodadas: a quarta vai sem ferramentas; se o modelo ainda pedir uma, não há texto.
    let w = World::new("rounds", vec![project_status(), project_status(), project_status(), project_status()], Vec::new(), false, |_| {});
    w.gate.analysis_open.add_permits(10);
    let done = w.spawn(Cancel::new()).await.expect("começou").await.expect("tarefa");
    assert!(matches!(done.ended, Ended::Skipped("rounds")), "{:?}", done.ended);
    let requests = w.gate.analysis.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[3].tools.is_empty() && !requests[2].tools.is_empty());
    assert_eq!(w.analysis_turns("turn.invalid")[0]["reason"], "rounds");

    // Teto de tokens (1.000; cada rodada simulada gasta 550): a terceira rodada é a última.
    let w = World::new("tokens", vec![project_status(), project_status(), say("cenário curto")], Vec::new(), false, |cfg| {
        cfg.nanty.analysis.max_tokens = 1_000;
    });
    w.gate.analysis_open.add_permits(10);
    let done = w.spawn(Cancel::new()).await.expect("começou").await.expect("tarefa");
    assert_eq!(reply_text(&done), Some("cenário curto"));
    let requests = w.gate.analysis.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].tools.is_empty() && !requests[1].tools.is_empty());
}

#[tokio::test]
async fn the_shared_budget_stops_the_analysis_before_and_during() {
    // 600 tokens no dia: a primeira rodada (550) cabe, a segunda passa, e a terceira não sai.
    // A fatia das análises (T12) fica inteira aqui: o teste é do orçamento do dia.
    let w = World::new("budget", vec![project_status(), project_status(), say("nunca")], Vec::new(), false, |cfg| {
        cfg.nanty.daily_budget_tokens = 600;
        cfg.nanty.analysis.budget_pct = 100;
    });
    w.gate.analysis_open.add_permits(10);
    let done = w.spawn(Cancel::new()).await.expect("começou").await.expect("tarefa");
    assert!(matches!(done.ended, Ended::Skipped("budget")), "{:?}", done.ended);
    assert_eq!(w.gate.analysis.requests().len(), 2);
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!((spend.input_tokens, spend.output_tokens), (1000, 100), "a análise gasta do mesmo orçamento do dia");
    // Sem orçamento, nem começa.
    assert_eq!(w.spawn(Cancel::new()).await.err(), Some("budget"));
    assert_eq!(w.gate.analysis.requests().len(), 2);
    let reasons: Vec<Value> = w.analysis_turns("turn.skipped").iter().map(|p| p["reason"].clone()).collect();
    assert_eq!(reasons, vec![json!("budget"), json!("budget")]);
}

#[test]
fn analysis_limits_are_written_only_when_changed() {
    let cfg = Config::default();
    let limits = Limits::analysis(&cfg);
    assert_eq!((limits.deadline, limits.rounds, limits.tokens), (Duration::from_secs(90), 4, 25_000));
    assert!(!toml::to_string(&cfg).expect("ser").contains("[nanty"));
    let mut cfg = Config::default();
    cfg.nanty.analysis.max_rounds = 3;
    let text = toml::to_string(&cfg).expect("ser");
    assert_eq!(text.split("[nanty.analysis]").nth(1).expect("seção").trim(), "max_rounds = 3", "{text}");
    let back: Config = toml::from_str(&text).expect("de");
    assert_eq!(back.nanty.analysis, cfg.nanty.analysis);
    // A faixa segura.
    let mut odd = Config::default().nanty.analysis;
    (odd.deadline_secs, odd.max_rounds, odd.max_tokens) = (0, 0, 0);
    assert_eq!((odd.deadline(), odd.rounds(), odd.tokens()), (Duration::from_secs(1), 1, 1_000));
}

// ---- O cancelamento ----

#[tokio::test]
async fn cancelling_stops_the_analysis_and_writes_nothing() {
    // Pelo sinal: a espera do modelo termina na hora, sem a licença do portão.
    let w = World::new("cancel", vec![say("nunca")], Vec::new(), false, |_| {});
    let cancel = Cancel::new();
    let running = w.spawn(cancel.clone()).await.expect("começou");
    w.gate.analysis_in.notified().await;
    cancel.cancel();
    let done = running.await.expect("tarefa");
    assert!(matches!(done.ended, Ended::Skipped("cancelled")), "{:?}", done.ended);
    assert_eq!(w.analysis_turns("turn.skipped")[0]["reason"], "cancelled");
    let nanty = w.nanty();
    let st = nanty.snapshot();
    assert!(nanty.conversation().is_empty() && st.cards.is_empty(), "nada gravado");
    assert_eq!(st.spend_on(&nanty.now().day_key()).input_tokens, 0, "sem resposta, sem gasto");
    assert!(!nanty.analysis().running());

    // Pela condição: o caso se resolveu enquanto o modelo pensava; a resposta dele não segue, e a
    // ferramenta que ela pedia não roda.
    let w = World::new("resolved", vec![project_status(), say("nunca")], Vec::new(), false, |_| {});
    let resolved = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&resolved);
    let running = w.spawn(Cancel::when(move || flag.load(Ordering::SeqCst))).await.expect("começou");
    w.gate.analysis_in.notified().await;
    resolved.store(true, Ordering::SeqCst);
    w.gate.analysis_open.add_permits(10);
    let done = running.await.expect("tarefa");
    assert!(matches!(done.ended, Ended::Skipped("cancelled")), "{:?}", done.ended);
    assert_eq!(w.gate.analysis.requests().len(), 1);
    assert!(done.session.read.is_empty() && done.session.seen.is_empty(), "a ferramenta não rodou");
    assert!(w.nanty().conversation().is_empty());
    // Já resolvido: nem começa.
    assert_eq!(w.spawn(Cancel::when(|| true)).await.err(), Some("cancelled"));
    let reasons: Vec<Value> = w.analysis_turns("turn.skipped").iter().map(|p| p["reason"].clone()).collect();
    assert_eq!(reasons, vec![json!("cancelled"), json!("cancelled")]);
}

// ---- O avatar ----

#[tokio::test]
async fn the_avatar_thinks_until_both_the_conversation_and_the_analysis_end() {
    let w = World::new("avatar", vec![say("cenário")], vec![say("Oi!")], true, |_| {});
    let analysis = w.spawn(Cancel::new()).await.expect("começou");
    w.gate.analysis_in.notified().await;
    assert_eq!(w.avatar(), Avatar::Thinking);

    let state = w.state.clone();
    let conversation = tokio::spawn(async move { say_in(&state, "oi").await });
    w.gate.other_in.notified().await;
    assert_eq!((w.avatar(), w.nanty().thinking_turns()), (Avatar::Thinking, 2));

    // A conversa termina primeiro: a análise segue pensando.
    w.gate.other_open.add_permits(1);
    let answer = conversation.await.expect("tarefa");
    assert_eq!(answer.text.as_deref(), Some("Oi!"));
    assert_eq!((w.avatar(), w.nanty().thinking_turns()), (Avatar::Thinking, 1));

    w.gate.analysis_open.add_permits(1);
    assert_eq!(reply_text(&analysis.await.expect("tarefa")), Some("cenário"));
    assert_eq!(w.nanty().thinking_turns(), 0);
    assert_ne!(w.avatar(), Avatar::Thinking);
}
