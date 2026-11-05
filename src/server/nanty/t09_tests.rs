//! Testes da visão do Nants para o Nanty (spec 141 T09, G7 e G8, C-09 e C-10), só com o modelo
//! simulado ou sem modelo (nunca chave real):
//!
//! - `nants_docs` e `system_status` nunca levam segredo, caminho do usuário, IP, URL, id ou nome de
//!   conexão nem projeto não observado (a varredura), e `nants_docs` não contamina o turno;
//! - o sinal de falha do Nants (`nants_environment`, T03) chega como fato ao resumo da manhã, aos
//!   avisos e às ferramentas; nada oferece retomar o projeto preso, e a reserva sem modelo diz que o
//!   defeito é do Nants.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::actions::{self, ActionId};
use super::agent::Session;
use super::chat::tests::{base_config, key};
use super::chat::{person_message, reply};
use super::signals::SignalKind;
use super::store::{Channel, DecidedBy};
use super::{act, digest, observe, render, tools, turn, InstanceStatus, Nanty};
use crate::agents::llm_client::ToolCall;
use crate::config::{Config, Connection, ConnectionKind, Project};
use crate::i18n::{tr, Lang};
use crate::server::state::AppState;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-141t09-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

/// Uma segunda chave, montada em tempo de execução.
fn other_key() -> String {
    format!("{}-{}{}", "sk", "empresa", "q".repeat(26))
}

/// Projeto em Docker e com aprovação automática, com a execução pausada: a T01 feita e a T02 BLOCKED pelo `code`. O
/// texto livre da tarefa leva um caminho do usuário, que nunca sai.
fn write_blocked(root: &Path, code: &str) {
    let spec = root.join(".sdd/specs/001-demo");
    std::fs::create_dir_all(spec.join("tasks")).expect("mkdir");
    std::fs::write(
        root.join(".sdd/settings.yaml"),
        "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\nenvironment: \"docker\"\nyolo: true\n",
    )
    .expect("settings");
    std::fs::write(
        spec.join("spec.md"),
        "---\nid: \"001-demo\"\ntitle: \"Demo\"\nstatus: \"IN_PROGRESS\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-30T10:00:00Z\"\n---\n\n## Contexto\nx\n",
    )
    .expect("spec");
    for (id, status, extra) in [
        ("T01", "DONE", String::new()),
        ("T02", "BLOCKED", format!("next_action_code: \"{code}\"\nnext_action: \"os testes caíram em /home/fulano/.nants-home\"\n")),
    ] {
        std::fs::write(
            spec.join(format!("tasks/{id}.md")),
            format!("---\nid: \"{id}\"\nspec_id: \"001-demo\"\ntitle: \"Tarefa\"\nstatus: \"{status}\"\ndepends_on: []\nbranch: \"\"\nattempts: 1\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\n{extra}allowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n"),
        )
        .expect("task");
    }
}

struct World {
    dir: PathBuf,
    state: AppState,
}

impl World {
    /// `p1` observado, pausado pelo `code`, com uma chave no nome; `oculto-p9x` excluído, com caminho
    /// do usuário. Os papéis passam por uma conexão com id, nome e endereço que nunca saem.
    fn new(tag: &str, code: &str, edit: impl FnOnce(&mut Config)) -> Self {
        let dir = temp(tag);
        let mut cfg = base_config(&dir, &[("p1", "Loja")]);
        cfg.projects[0].name = format!("Loja {}", key());
        cfg.projects.push(Project {
            id: "oculto-p9x".into(),
            name: "PROJETO-NAO-OBSERVADO".into(),
            path: "/home/fulano/projetos/oculto".into(),
            created_at: "2026-10-30T00:00:00Z".into(),
        });
        cfg.nanty.excluded_projects = vec!["oculto-p9x".into()];
        cfg.connections.push(Connection {
            id: "conn-SEGREDO-ID".into(),
            name: "Conta da Empresa X".into(),
            base_url: "https://10.20.30.40:8443/v1".into(),
            api_key: other_key(),
            wire_api: Default::default(),
            kind: ConnectionKind::Anthropic,
            base_url_override: Some("https://proxy.empresa.example/anthropic".into()),
        });
        cfg.agents.architect.connection_id = "conn-SEGREDO-ID".into();
        cfg.agents.architect.model = "claude-arq-1".into();
        cfg.agents.developer.connection_id = "main".into();
        cfg.agents.developer.model = "https://modelo.example/dev".into();
        cfg.agents.reviewer.connection_id = "sumiu".into();
        cfg.agents.reviewer.model = "rev-1".into();
        edit(&mut cfg);
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(super::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        write_blocked(&dir.join("p1"), code);
        Self { dir, state }
    }

    fn nanty(&self) -> Arc<Nanty> {
        Arc::clone(self.state.nanty())
    }

    async fn observe(&self) {
        let cfg = self.state.config().read().await.clone();
        let signals = observe::observe_projects(&self.state, &cfg).await.expect("observou");
        self.nanty().observe(&cfg.nanty, &signals, &SignalKind::PROJECT_CONDITIONS);
    }

    async fn tool(&self, name: &str, args: Value, session: &mut Session) -> String {
        let cfg = self.state.config().read().await.clone();
        let secrets = self.state.secret_set().await;
        let call = ToolCall { id: "c1".into(), name: name.into(), arguments: args.to_string() };
        tools::call(&self.state, &self.nanty(), &cfg, Lang::PtBr, &secrets, &call, session).await
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---- A varredura ----

#[tokio::test]
async fn system_status_and_nants_docs_never_carry_secrets_paths_addresses_or_hidden_projects() {
    let w = World::new("sweep", "tests_failed", |_| {});
    w.observe().await;
    let mut session = Session::new(Channel::Web, Default::default(), Lang::PtBr);
    let system = w.tool(tools::SYSTEM_STATUS, json!({}), &mut session).await;
    let mut outputs = vec![system.clone(), w.tool(tools::PROJECT_STATUS, json!({"project_id": "p1"}), &mut session).await];
    for query in ["", "security network address", "recovery escalation", "Telegram token pairing", "config.toml connection key", "zzqqxx"] {
        outputs.push(w.tool(tools::NANTS_DOCS, json!({ "query": query }), &mut session).await);
    }
    assert!(!session.is_tainted(), "a documentação e o estado do servidor não contaminam o turno");
    let dir = w.dir.to_str().expect("utf-8").to_string();
    let forbidden = [
        key(),
        other_key(),
        dir,
        "/home/".into(),
        "10.20.30.40".into(),
        "127.0.0.1".into(),
        "http://".into(),
        "https://".into(),
        "modelo.example".into(),
        "proxy.empresa".into(),
        "conn-SEGREDO-ID".into(),
        "Conta da Empresa X".into(),
        "oculto-p9x".into(),
        "PROJETO-NAO-OBSERVADO".into(),
        "001-demo".into(),
    ];
    for out in &outputs {
        for bad in &forbidden {
            assert!(!out.contains(bad.as_str()), "vazou {bad:?} em:\n{out}");
        }
    }
    // Os campos enumerados estão lá: a versão, o tipo de conexão e o modelo de cada papel, e o
    // projeto observado com o ambiente e a aprovação automática (`yolo`).
    let v: Value = serde_json::from_str(&system).expect("JSON");
    assert_eq!(v["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(v["roles"]["architect"], json!({"connection": "anthropic", "model": "claude-arq-1"}));
    assert_eq!(v["roles"]["developer"], json!({"connection": "api_key", "model": null}), "endereço no lugar do modelo fica de fora");
    assert_eq!(v["roles"]["reviewer"], json!({"connection": null, "model": "rev-1"}));
    assert_eq!(v["roles"]["nanty"]["model"], json!("m-nanty"));
    let projects = v["projects"].as_array().expect("lista");
    assert_eq!(projects.len(), 1, "só o observado: {v}");
    let p1 = &projects[0];
    assert_eq!((p1["id"].as_str(), p1["name"].as_str()), (Some("p1"), Some("Loja ***")), "{v}");
    assert_eq!((p1["environment"].as_str(), p1["yolo"].as_bool(), p1["phase"].as_str()), (Some("docker"), Some(true), Some("paused")), "{v}");
    assert_eq!(p1["reason"], json!("tests_failed"));
    assert!(p1.get("nants_fault").is_none(), "{v}");
    assert!(v["machine"]["disk_unmeasured"].as_bool().unwrap_or(false), "a sonda muda dos testes não mede: {v}");
}

// ---- O sinal de falha do Nants ----

#[tokio::test]
async fn the_nants_fault_reaches_the_digest_notices_and_tools_as_a_fact_and_nothing_offers_to_resume() {
    let w = World::new("fault", crate::agents::runner::BLOCK_NANTS_ENVIRONMENT, |c| {
        c.nanty.model.clear();
        c.projects[0].name = "Loja".into();
    });
    w.observe().await;
    let snap = w.nanty().snapshot();
    let alert = snap.alerts.values().find(|a| a.kind == SignalKind::RunBlocked.as_str()).expect("a condição de pausa");
    assert_eq!(alert.facts.get("reason"), Some(&json!("nants_environment")));

    // As ferramentas: o dado leva o sinal.
    let mut session = Session::new(Channel::Web, Default::default(), Lang::PtBr);
    let status: Value = serde_json::from_str(&w.tool(tools::PROJECT_STATUS, json!({"project_id": "p1"}), &mut session).await).expect("JSON");
    assert_eq!((status["status"]["nants_fault"].as_bool(), status["status"]["reason"].as_str()), (Some(true), Some("nants_environment")), "{status}");
    let system: Value = serde_json::from_str(&w.tool(tools::SYSTEM_STATUS, json!({}), &mut session).await).expect("JSON");
    assert_eq!(system["projects"][0]["nants_fault"], json!(true), "{system}");

    // Nada oferece retomar: nem a sugestão da condição, nem o que a conversa pode propor.
    let cfg = w.state.config().read().await.clone();
    assert!(actions::suggest(alert, &cfg, &snap.retries).is_none());
    let offers = act::requestable(&w.state, &cfg, &snap, Some("p1"));
    assert!(offers.iter().all(|o| o.action != ActionId::ResumeRun), "{offers:?}");
    let p1_cards: Vec<_> = snap.cards.iter().filter(|c| c.project_id.as_deref() == Some("p1")).cloned().collect();
    assert!(!p1_cards.is_empty(), "o aviso da pausa saiu");
    for card in &p1_cards {
        assert_ne!(card.action.as_deref(), Some(ActionId::ResumeRun.as_str()), "o cartão não oferece retomar: {card:?}");
        // O aviso leva o sinal ao modelo como fato.
        let fact = turn::SignalFact::of_card(card).expect("aviso");
        assert!(fact.nants_fault, "{fact:?}");
    }
    let notice = turn::notice_input(&p1_cards, &snap, "2026-11-05");
    assert!(notice.to_json().contains("\"nants_fault\":true"), "{}", notice.to_json());

    // O resumo da manhã: o projeto com o sinal leva o código no dado do modelo.
    let now = w.nanty().now();
    let facts = digest::gather(&w.state, w.state.nanty(), &now).await;
    assert_eq!(facts.nants_faults, vec![digest::NantsFault { project_id: "p1".into(), name: "Loja".into() }]);
    let input = digest::digest_input(&facts, &snap);
    let signal = input.signals.iter().find(|s| s.project_id.as_deref() == Some("p1") && s.kind == "run_blocked").expect("o sinal no resumo");
    assert!(signal.nants_fault && signal.facts.get("reason") == Some(&json!("nants_environment")), "{signal:?}");
    assert!(input.to_json().contains("\"nants_fault\":true"));

    // A reserva sem modelo diz que é do Nants, nos dois idiomas, e não recomenda retomar.
    let message = digest::compose(&facts, &cfg.nanty, "m".into(), now.epoch_ms).expect("resumo");
    let codes: Vec<&str> = message.params["lines"].as_array().expect("linhas").iter().filter_map(|l| l["code"].as_str()).collect();
    assert!(codes.contains(&"nanty.digest.nants_fault"), "{codes:?}");
    for lang in [Lang::PtBr, Lang::En] {
        let shown = render::message_text(lang, &message);
        assert!(shown.contains(&tr(lang, "nanty.digest.nants_fault", &[("project", "Loja")])), "{shown}");
    }
    assert!(render::message_text(Lang::PtBr, &message).contains("defeito do ambiente do próprio Nants"));

    // A reserva da conversa, sem modelo, conta o motivo em frase.
    let secrets = w.state.secret_set().await;
    let person = person_message(&w.nanty(), &cfg.nanty, &secrets, Channel::Web, "como está a Loja?").expect("mensagem");
    let answer = reply(&w.state, &person).await.expect("resposta");
    let shown = render::message_text(Lang::PtBr, &answer);
    assert!(shown.contains(&tr(Lang::PtBr, "nanty.reason.nants_environment", &[])), "{shown}");
}

/// Os prompts tratam o sinal como fato e nunca recomendam retomar; a conversa conhece as duas
/// ferramentas novas.
#[test]
fn the_prompts_take_the_nants_fault_as_a_fact_and_know_the_two_tools() {
    for lang in [Lang::PtBr, Lang::En] {
        let notice = crate::agents::prompts::nanty_system(lang);
        assert!(notice.contains("`nants_fault`") && notice.contains("nunca recomende retomar"), "{lang:?}");
        let chat = crate::agents::prompts::nanty_chat_system(lang, false);
        assert!(chat.contains("`nants_docs`") && chat.contains("`system_status`") && chat.contains("não sugira retomar"), "{lang:?}");
        let scenario = crate::agents::prompts::nanty_scenario_system(lang, 900, true);
        assert!(scenario.contains("`nants_fault`") && scenario.contains("nunca sugira"), "{lang:?}");
    }
    let names: Vec<String> = crate::agents::prompts::nanty_chat_tools().into_iter().map(|t| t.name).collect();
    assert!(names.iter().any(|n| n == tools::NANTS_DOCS));
    assert!(tools::Mode::Conversation.allows(tools::NANTS_DOCS) && !tools::Mode::Analysis.allows(tools::NANTS_DOCS));
}

// ---- Pendências da revisão (spec 141 T12) ----

/// Disparo falso: só conta as chamadas, para provar que nada retomou.
#[derive(Default)]
struct CountingStarter(std::sync::atomic::AtomicUsize);

impl act::RunStarter for CountingStarter {
    fn start(&self, _: AppState, _: String, _: String, _: act::StartMode) -> act::StartFuture {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

/// O cartão `resume_run` nasceu com o motivo `tests_failed`; depois o motivo virou
/// `nants_environment` no disco, sem nova varredura. O clique confere o motivo de agora e recusa com
/// código, sem chamar o `runs::start`.
#[tokio::test]
async fn an_old_resume_card_is_refused_with_a_code_once_the_reason_is_a_nants_fault() {
    let w = World::new("old-resume", "tests_failed", |c| c.projects[0].name = "Loja".into());
    let starter = Arc::new(CountingStarter::default());
    w.nanty().set_starter(Arc::clone(&starter) as Arc<dyn act::RunStarter>);
    w.observe().await;
    act::tick(&w.state, &w.nanty()).await;
    let card = w
        .nanty()
        .snapshot()
        .cards
        .into_iter()
        .find(|c| c.action.as_deref() == Some(ActionId::ResumeRun.as_str()))
        .expect("o cartão de retomar, com o motivo antigo");
    write_blocked(&w.dir.join("p1"), crate::agents::runner::BLOCK_NANTS_ENVIRONMENT);
    let token = card.token.clone().expect("token");
    let out = act::decide(&w.state, &card.id, &token, act::Answer::Yes, DecidedBy::Web).await.expect("decide");
    assert_eq!(out.outcome, "failed", "{out:?}");
    assert_eq!(out.params.get("error"), Some(&json!("nanty_resume_nants_fault")), "{out:?}");
    assert_eq!(starter.0.load(std::sync::atomic::Ordering::SeqCst), 0, "nada retomou");
    for lang in [Lang::PtBr, Lang::En] {
        let text = act::outcome_text(lang, &out);
        assert!(text.contains(&tr(lang, "serverError.nanty_resume_nants_fault", &[])), "{text}");
    }
}

/// O mesmo cartão, com o motivo ainda do projeto: o clique retoma como sempre.
#[tokio::test]
async fn a_resume_card_still_resumes_when_the_reason_is_the_project() {
    let w = World::new("resume-ok", "tests_failed", |c| c.projects[0].name = "Loja".into());
    let starter = Arc::new(CountingStarter::default());
    w.nanty().set_starter(Arc::clone(&starter) as Arc<dyn act::RunStarter>);
    w.observe().await;
    act::tick(&w.state, &w.nanty()).await;
    let card = w
        .nanty()
        .snapshot()
        .cards
        .into_iter()
        .find(|c| c.action.as_deref() == Some(ActionId::ResumeRun.as_str()))
        .expect("o cartão de retomar");
    let token = card.token.clone().expect("token");
    let out = act::decide(&w.state, &card.id, &token, act::Answer::Yes, DecidedBy::Web).await.expect("decide");
    assert_eq!(out.outcome, "done", "{out:?}");
    assert_eq!(starter.0.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// O `system_status` corta os projetos ANTES de ler os fatos: com o teto em 1, só o primeiro
/// observado é lido.
#[tokio::test]
async fn observed_facts_cut_before_reading() {
    let w = World::new("cut", "tests_failed", |c| {
        let mut extra = c.projects[0].clone();
        extra.id = "p2".into();
        extra.name = "Outra".into();
        c.projects.push(extra);
    });
    let cfg = w.state.config().read().await.clone();
    let all = observe::observed_facts(&w.state, &cfg, usize::MAX).await;
    assert_eq!(all.iter().map(|(p, _)| p.id.as_str()).collect::<Vec<_>>(), ["p1", "p2"]);
    let cut = observe::observed_facts(&w.state, &cfg, 1).await;
    assert_eq!(cut.iter().map(|(p, _)| p.id.as_str()).collect::<Vec<_>>(), ["p1"]);
}
