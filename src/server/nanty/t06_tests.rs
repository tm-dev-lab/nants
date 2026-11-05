//! Testes do texto natural do Nanty (spec 139 T06, G4 e C-11), só com o modelo simulado (nunca chave
//! real):
//!
//! - as saídas das ferramentas e as reservas do dicionário não têm snake_case, "0 tentativas" nem
//!   chaves internas, nos dois idiomas;
//! - o aviso e o resumo com o texto do modelo não repetem as linhas fixas; o porquê do aviso segue no
//!   cartão, e o do resumo fica como dado para a janela (spec 141 T10);
//! - o `/status` e o `/resumo` são escritos pelo modelo quando há modelo e orçamento, a partir dos
//!   mesmos fatos (sem zeros), e caem na reserva sem ele.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::agent::{tests::call, tests::say, tests::Scripted, Session};
use super::chat::tests::{base_config, write_project, FakeModel};
use super::chat::{person_message, reply};
use super::digest::{self, DigestFacts};
use super::render;
use super::signals::{Signal, SignalKind};
use super::store::{Channel, ConversationMessage, DigestItem};
use super::turn::{self, TurnOutcome};
use super::{tools, InstanceStatus, Nanty};
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{Role, ToolCall};
use crate::config::Config;
use crate::i18n::{en, pt_br, Lang};
use crate::server::state::AppState;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-139t06-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

/// Projeto com a execução pausada: a T01 feita e a T02 BLOCKED pelos testes, com duas tentativas.
fn write_blocked(root: &Path) {
    let spec = root.join(".sdd/specs/001-demo");
    std::fs::create_dir_all(spec.join("tasks")).expect("mkdir");
    std::fs::write(root.join(".sdd/settings.yaml"), "base_branch: \"main\"\nmerge_strategy: \"squash\"\ntest_command: \"true\"\n")
        .expect("settings");
    std::fs::write(
        spec.join("spec.md"),
        "---\nid: \"001-demo\"\ntitle: \"Demo\"\nstatus: \"IN_PROGRESS\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-30T10:00:00Z\"\n---\n\n## Contexto\nx\n",
    )
    .expect("spec");
    for (id, status, code) in [("T01", "DONE", ""), ("T02", "BLOCKED", "next_action_code: \"tests_failed\"\n")] {
        std::fs::write(
            spec.join(format!("tasks/{id}.md")),
            format!("---\nid: \"{id}\"\nspec_id: \"001-demo\"\ntitle: \"Tarefa\"\nstatus: \"{status}\"\ndepends_on: []\nbranch: \"\"\nattempts: 2\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\n{code}allowed_scope:\n  - \"src/**\"\n---\n\n## Objetivo\nx\n"),
        )
        .expect("task");
    }
}

struct World {
    dir: PathBuf,
    state: AppState,
}

impl World {
    /// `p1` (Loja, com o plano esperando) e `p14` (Test 14, pausado numa tarefa bloqueada).
    fn new(tag: &str, model: Option<Arc<dyn ChatBackend>>, edit: impl FnOnce(&mut Config)) -> Self {
        let dir = temp(tag);
        let mut cfg = base_config(&dir, &[("p1", "Loja"), ("p14", "Test 14")]);
        edit(&mut cfg);
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(super::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        if let Some(model) = model {
            state.nanty().set_backend(model);
        }
        write_project(&dir.join("p1"), "Vitrine nova", "LOG-LOJA", None);
        write_blocked(&dir.join("p14"));
        Self { dir, state }
    }

    fn nanty(&self) -> Arc<Nanty> {
        Arc::clone(self.state.nanty())
    }

    async fn say(&self, text: &str) -> ConversationMessage {
        let secrets = self.state.secret_set().await;
        let cfg = self.state.config().read().await.nanty.clone();
        let person = person_message(&self.nanty(), &cfg, &secrets, Channel::Web, text).expect("mensagem");
        reply(&self.state, &person).await.expect("resposta")
    }

    async fn tool(&self, lang: Lang, name: &str, args: Value) -> String {
        let cfg = self.state.config().read().await.clone();
        let secrets = self.state.secret_set().await;
        let mut session = Session::new(Channel::Web, Default::default(), lang);
        let call = ToolCall { id: "c1".into(), name: name.into(), arguments: args.to_string() };
        tools::call(&self.state, &self.nanty(), &cfg, lang, &secrets, &call, &mut session).await
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Os ids que a saída pode citar (C-04: as frases do dicionário mais os ids): os do catálogo de
/// ações, os dos projetos e os dos cartões. Fora deles, nada com sublinhado.
fn without_ids(text: &str, ids: &[String]) -> String {
    let mut out = text.to_string();
    let mut all: Vec<String> = super::actions::CATALOG.iter().map(|e| e.id.as_str().to_string()).collect();
    all.extend(ids.iter().cloned());
    all.sort_by_key(|id| std::cmp::Reverse(id.len()));
    for id in all {
        out = out.replace(&id, "ID");
    }
    out
}

/// O texto sem os marcadores `{x}` do dicionário (o nome do parâmetro não é dito).
fn without_placeholders(text: &str) -> String {
    let mut plain = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    plain
}

/// Palavras com sublinhado (snake_case).
fn snake_words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| w.contains('_') && w.chars().any(char::is_alphanumeric))
        .map(str::to_string)
        .collect()
}

/// Chaves e códigos internos que nunca saem numa frase.
const INTERNAL: [&str; 9] = ["spec_id", "task_id", "project_id", "next_action", "attempts:", "reason:", "stop:", "role:", "tests_failed"];

fn assert_human(text: &str, ids: &[String], what: &str) {
    let plain = without_placeholders(&without_ids(text, ids));
    let snake = snake_words(&plain);
    assert!(snake.is_empty(), "{what}: snake_case {snake:?} em:\n{text}");
    for bad in INTERNAL {
        assert!(!plain.contains(bad), "{what}: chave interna {bad:?} em:\n{text}");
    }
    let lower = plain.to_lowercase();
    // O zero sozinho ("0 tentativas", "Esperando você: 0"); `10 de 20` não conta.
    for zero in ["0 tentativa", "0 attempt", "0 de ", "0 of "] {
        for (i, _) in lower.match_indices(zero) {
            let prev = i.checked_sub(1).map(|j| lower.as_bytes()[j]);
            assert!(prev.is_some_and(|b| b.is_ascii_digit() || b == b'.' || b == b','), "{what}: zero cru {zero:?} em:\n{text}");
        }
    }
    for zero in [": 0.", ": 0,", ": 0\n"] {
        assert!(!lower.contains(zero) && !lower.ends_with(": 0"), "{what}: zero cru {zero:?} em:\n{text}");
    }
}

// ---- Saídas de ferramenta e reservas sem código cru ----

#[test]
fn every_nanty_phrase_in_both_dictionaries_speaks_without_snake_case_or_raw_zero() {
    for (lang, messages) in [(Lang::PtBr, pt_br::MESSAGES), (Lang::En, en::MESSAGES)] {
        for (key, text) in messages.iter().filter(|(k, _)| k.starts_with("nanty.") && !k.starts_with("nanty.log.")) {
            assert_human(text, &[], &format!("{lang:?} {key}"));
        }
    }
}

/// Spec 141 T09 (C-09, C-10): `project_status` e `system_status` devolvem dados com códigos, sem zero
/// e sem o id da spec; as outras ferramentas seguem em frases curtas, sem código nem zero.
#[tokio::test]
async fn status_tools_return_data_with_codes_and_the_others_short_phrases_in_both_languages() {
    let w = World::new("tools", None, |_| {});
    // Um cartão aberto e algo feito hoje, para o `nanty_activity` ter linhas.
    let cfg = w.state.config().read().await.nanty.clone();
    w.nanty().observe(&cfg, &[Signal::new(SignalKind::DiskLow, None).fact("min_gb", 5).fact("count", 1).extra("free_gb", 2)], &[]);
    let cards: Vec<String> = w.nanty().snapshot().cards.iter().map(|c| c.id.clone()).collect();
    let ids: Vec<String> = ["p1", "p14", "001-demo", "T01", "T02"].iter().map(|s| (*s).to_string()).chain(cards).collect();
    for lang in [Lang::PtBr, Lang::En] {
        let status: Value = serde_json::from_str(&w.tool(lang, "project_status", json!({"project_id": "p14"})).await).expect("JSON");
        let s = &status["status"];
        assert_eq!((s["reason"].as_str(), s["attempts"].as_u64(), s["max_attempts"].as_u64()), (Some("tests_failed"), Some(2), Some(3)), "{status}");
        assert!(s.get("spec_id").is_none() && s.get("cost_usd").is_none(), "{status}");
        let system: Value = serde_json::from_str(&w.tool(lang, "system_status", json!({})).await).expect("JSON");
        assert_eq!(system["version"], json!(env!("CARGO_PKG_VERSION")));
        let p14 = system["projects"].as_array().and_then(|l| l.iter().find(|p| p["id"] == "p14")).expect("p14");
        assert_eq!((p14["phase"].as_str(), p14["reason"].as_str()), (Some("paused"), Some("tests_failed")), "{system}");
        for (name, args) in [
            ("project_status", json!({"project_id": "nada"})),
            ("project_status", json!({})),
            ("nanty_activity", json!({})),
            ("set_silence", json!({"hours": 99})),
            ("propose_action", json!({"action_id": "approve_plan", "project_id": "p14"})),
            ("propose_action", json!({"action_id": "approve_plan", "project_id": "p1"})),
        ] {
            let out = w.tool(lang, name, args.clone()).await;
            assert_human(&out, &ids, &format!("{lang:?} {name} {args}"));
        }
    }
}

#[tokio::test]
async fn the_reserves_say_each_fact_in_a_sentence_and_never_a_zero() {
    let w = World::new("reserves", None, |c| c.nanty.model = String::new());
    for (text, lang) in [("/status", Lang::PtBr), ("/resumo", Lang::PtBr), ("/summary", Lang::En)] {
        let message = w.say(text).await;
        assert!(message.text.is_none(), "sem modelo, a reserva: {message:?}");
        let shown = render::message_text(lang, &message);
        assert_human(&shown, &[], text);
        assert!(!shown.contains(" 0 ") && !shown.contains(": 0"), "{text}: {shown}");
    }
    // A reserva de um projeto citado, sem modelo: a fase e o andamento, em frases.
    let message = w.say("como está o Test 14?").await;
    let shown = render::message_text(Lang::PtBr, &message);
    assert!(shown.starts_with("Estou sem o modelo agora, mas posso contar o que sei do projeto Test 14:"), "{shown}");
    assert!(shown.contains("1 de 2 tarefas já está pronta."), "{shown}");
    assert_human(&shown, &["001-demo".into(), "T02".into()], "reserva do projeto");
    // O resumo sem nada pausado, esperando ou guardado não fala em zero.
    let facts = DigestFacts { projects: 2, running: 0, paused: 0, waiting: 0, disk_low: true, disk_free_gb: Some(3), ..DigestFacts::default() };
    let message = digest::compose(&facts, &crate::config::NantyConfig::default(), "m".into(), 1).expect("resumo");
    for lang in [Lang::PtBr, Lang::En] {
        let shown = render::message_text(lang, &message);
        assert_human(&shown, &[], "resumo");
        assert!(!shown.contains(" 0 ") && !shown.starts_with('0'), "{shown}");
    }
    // Sem preço (o plano, por exemplo), o custo vira só os tokens: nada de "US$ 0,00".
    let facts = DigestFacts { waiting: 1, tokens_yesterday: 900, unpriced_yesterday: true, ..DigestFacts::default() };
    let message = digest::compose(&facts, &crate::config::NantyConfig::default(), "m".into(), 1).expect("resumo");
    let shown = render::message_text(Lang::PtBr, &message);
    assert!(shown.contains("Ontem os projetos usaram 900 tokens no total.") && !shown.contains("US$"), "{shown}");
}

// ---- Aviso e resumo com o texto do modelo ----

#[tokio::test]
async fn the_notice_with_model_text_does_not_repeat_the_fixed_lines() {
    let reply = r#"{"summary": "O disco está quase cheio; vale liberar espaço antes do próximo build.", "actions": []}"#;
    let model = FakeModel::new(&[reply]);
    let w = World::new("notice", Some(Arc::clone(&model) as Arc<dyn ChatBackend>), |_| {});
    let cfg = w.state.config().read().await.nanty.clone();
    w.nanty().observe(&cfg, &[Signal::new(SignalKind::DiskLow, None).fact("min_gb", 5).fact("count", 1).extra("free_gb", 2)], &[]);
    let outcome = turn::proactive(&w.state, &w.nanty()).await;
    assert!(matches!(outcome, Some(TurnOutcome::Reply(_))), "{outcome:?}");
    let card = w.nanty().snapshot().cards.into_iter().find(|c| c.code == "nanty.why.disk_low").expect("cartão");
    let message = w.nanty().conversation().pop().expect("mensagem do modelo");
    assert_eq!(message.card_ids, [card.id.clone()], "o porquê segue no cartão, do dicionário");
    for lang in [Lang::PtBr, Lang::En] {
        let shown = render::message_text(lang, &message);
        assert_eq!(shown, "O disco está quase cheio; vale liberar espaço antes do próximo build.");
        assert!(!shown.contains(&render::card_text(lang, &card)), "o aviso com modelo não repete o porquê fixo");
    }
    // A entrada do modelo não leva contagem zerada (zero não é fato) e o prompt pede para não
    // repetir o porquê nem dizer códigos.
    let request = &model.requests()[0];
    let input: Value = serde_json::from_str(&request.messages[1].content).expect("JSON");
    assert!(input["counts"].as_object().expect("contagens").values().all(|v| v.as_u64().is_some_and(|n| n > 0)), "{input}");
    let system = &request.messages[0].content;
    assert!(system.contains("não o repita") && system.contains("sem repetir códigos, ids, nomes de campo"), "{system}");
}

/// Spec 141 T10 (C-10): com o texto do modelo, o resumo é o selo e o texto dele; as linhas fixas de
/// contagem, custo e disco e o porquê de cada aviso guardado saem do texto (o porquê fica como dado,
/// para a janela).
#[test]
fn the_digest_with_model_text_is_the_seal_and_the_model_text_only() {
    let item = DigestItem {
        at_ms: 1,
        project_id: Some("p1".into()),
        group: "p1:task_delivered".into(),
        code: "nanty.why.task_delivered".into(),
        params: json!({"since": "02:00"}).as_object().cloned().unwrap_or_default(),
    };
    let facts = DigestFacts { projects: 2, running: 1, waiting: 1, items: vec![item], tokens_yesterday: 10, cost_yesterday_usd: 1.0, disk_free_gb: Some(40), ..DigestFacts::default() };
    let mut message = digest::compose(&facts, &crate::config::NantyConfig::default(), "m".into(), 1).expect("resumo");
    let counted: Vec<String> = message.params["lines"]
        .as_array()
        .expect("linhas")
        .iter()
        .map(|l| render::code_text(Lang::PtBr, l["code"].as_str().unwrap_or_default(), l["params"].as_object().unwrap_or(&serde_json::Map::new())))
        .collect();
    assert_eq!(counted.len(), 4, "contagem, espera, custo e disco: {counted:?}");
    let model = "Bom dia! Uma tarefa saiu de madrugada e um aviso espera você.";
    digest::with_model_text(&mut message, &facts, model.to_string());
    for (lang, seal) in [(Lang::PtBr, "Resumo da manhã"), (Lang::En, "Morning summary")] {
        assert_eq!(render::message_text(lang, &message), format!("{seal}\n{model}"));
    }
    assert!(message.params.get(digest::WHY).is_none(), "{:?}", message.params);
    assert_eq!(message.params[digest::HELD][0]["notices"][0]["kind"], "task_delivered");
    // Sem aviso guardado, só o selo e o texto.
    let facts = DigestFacts { paused: 1, ..DigestFacts::default() };
    let mut message = digest::compose(&facts, &crate::config::NantyConfig::default(), "m".into(), 1).expect("resumo");
    digest::with_model_text(&mut message, &facts, "Um projeto parou.".into());
    assert_eq!(render::message_text(Lang::En, &message), "Morning summary\nUm projeto parou.");
    assert!(message.params.is_empty(), "{:?}", message.params);
}

// ---- Os atalhos pelo modelo ----

#[tokio::test]
async fn status_and_summary_are_written_by_the_model_from_the_same_facts() {
    let status_reply = r#"{"summary": "Por aqui está calmo: nada espera você, e o Test 14 segue pausado.", "actions": []}"#;
    let summary_reply = r#"{"summary": "Agora o Test 14 é o único que pede atenção.", "actions": []}"#;
    let model = FakeModel::new(&[status_reply, summary_reply]);
    let w = World::new("commands", Some(Arc::clone(&model) as Arc<dyn ChatBackend>), |_| {});
    let observed = super::observe::observe_projects(&w.state, &w.state.config().read().await.clone()).await.expect("observou");
    let cfg = w.state.config().read().await.nanty.clone();
    w.nanty().observe(&cfg, &observed, &SignalKind::PROJECT_CONDITIONS);

    let status = w.say("/status").await;
    assert_eq!((status.code.as_deref(), status.text.as_deref()), (None, Some("Por aqui está calmo: nada espera você, e o Test 14 segue pausado.")));
    assert_eq!(status.lang.as_deref(), Some("pt-BR"));
    let summary = w.say("/resumo").await;
    assert_eq!((summary.code.as_deref(), summary.text.as_deref()), (None, Some("Agora o Test 14 é o único que pede atenção.")));
    assert!(summary.actions.is_empty());

    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    for (request, kind) in requests.iter().zip(["status", "summary"]) {
        assert!(request.response_json, "o turno com a entrada estruturada");
        assert_eq!(request.messages[0].role, Role::System);
        assert!(request.messages[0].content.contains("Em `status` e `summary`"), "o prompt do turno");
        assert!(request.messages[0].content.contains("português do Brasil"), "no idioma de quem falou");
        let input: Value = serde_json::from_str(&request.messages[1].content).expect("JSON");
        assert_eq!(input["kind"], kind);
        // Zero não é fato: nenhuma contagem zerada vai ao modelo.
        let counts = input["counts"].as_object().expect("contagens");
        assert!(counts.values().all(|v| v.as_u64().is_some_and(|n| n > 0)), "{input}");
        let signals = input["signals"].as_array().expect("sinais");
        assert!(signals.iter().any(|s| s["kind"] == "run_blocked" && s["project_id"] == "p14"), "{input}");
    }
    // O status leva o orçamento do dia (o que já foi gasto conta).
    let input: Value = serde_json::from_str(&requests[0].messages[1].content).expect("JSON");
    assert!(input["tokens"]["nanty_daily_budget"].as_u64().is_some_and(|n| n > 0), "{input}");

    // Sem orçamento, a reserva do dicionário, sem chamar o modelo.
    w.state.config().write().await.nanty.daily_budget_tokens = 1;
    let before = model.requests().len();
    let status = w.say("/status").await;
    assert_eq!(status.code.as_deref(), Some("nanty.chat.status"));
    assert_eq!(model.requests().len(), before);
    // Uma resposta fora do formato também cai na reserva.
    let w = World::new("commands-invalid", Some(Scripted::new(vec![say("não é JSON"), call("x", json!({}))]) as Arc<dyn ChatBackend>), |_| {});
    assert_eq!(w.say("/status").await.code.as_deref(), Some("nanty.chat.status"));
    assert_eq!(w.say("/summary").await.code.as_deref(), Some("nanty.chat.summary_quiet"));
}

/// Revisão 1 da T06 (F-04): as chaves que o `status_lines` monta com `format!` existem nos dois
/// idiomas para todo código das listas fixas; um código novo sem frase quebra aqui, e não vaza a
/// chave crua na reserva. Spec 141 T09: a reserva leva o motivo; a parada do provedor, o papel e o
/// que espera a pessoa vão ao modelo só em dados.
#[test]
fn every_built_status_key_exists_in_both_languages() {
    use super::observe::Phase;
    let mut keys: Vec<String> = crate::agents::runner::BLOCK_CODES.iter().map(|c| format!("nanty.reason.{c}")).collect();
    for phase in [Phase::Paused, Phase::ProviderStopped, Phase::PlanWaiting, Phase::Stalled, Phase::Running, Phase::Stopped] {
        keys.push(tools::phase_code(phase));
    }
    for lang in [Lang::PtBr, Lang::En] {
        for key in &keys {
            assert!(crate::i18n::lookup(lang, key).is_some(), "{lang:?}: falta {key}");
        }
        // As montadas com plural (`count`): as duas formas.
        for base in ["nanty.status.progress", "nanty.status.progress_none"] {
            for form in ["one", "other"] {
                assert!(crate::i18n::lookup(lang, &format!("{base}.{form}")).is_some(), "{lang:?}: falta {base}.{form}");
            }
        }
    }
}
