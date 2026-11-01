//! Testes do idioma do Nanty (spec 139 T05, G5 e C-13), só com o modelo simulado (nunca chave real):
//! a conversa e a reserva no idioma de quem falou, em pt e em en, com a interface no outro; o aviso
//! proativo seguindo a conversa (e o idioma escolhido vencendo); o `message.lang` gravado em toda
//! mensagem; a interface do resto do Nants sem mudança; e o corte da citação de terceiro, na mensagem
//! e na memória.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::Role;
use crate::config::{Config, NantyLanguage};
use crate::i18n::Lang;
use crate::server::nanty::agent::tests::{say, Scripted};
use crate::server::nanty::chat::tests::{base_config, write_project};
use crate::server::nanty::chat::{person_message, person_message_from, reply, MAX_QUOTE_CHARS};
use crate::server::nanty::signals::{Signal, SignalKind};
use crate::server::nanty::store::{Channel, ConversationMessage, Relayed, Speaker};
use crate::server::nanty::{nanty_lang, render, turn, InstanceStatus, Nanty};
use crate::server::state::AppState;

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-139t05-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

struct World {
    dir: PathBuf,
    state: AppState,
    model: Arc<Scripted>,
}

impl World {
    /// `p1` (Loja), a interface lembrada em `ui` e o modelo simulado.
    fn new(tag: &str, ui: &str, model: Arc<Scripted>, edit: impl FnOnce(&mut Config)) -> Self {
        let dir = temp(tag);
        let mut cfg = base_config(&dir, &[("p1", "Loja")]);
        cfg.nanty.lang = Some(ui.into());
        edit(&mut cfg);
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(crate::server::nanty::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        state.nanty().set_backend(Arc::clone(&model) as Arc<dyn ChatBackend>);
        write_project(&dir.join("p1"), "Vitrine", "nada", None);
        Self { dir, state, model }
    }

    fn nanty(&self) -> Arc<Nanty> {
        Arc::clone(self.state.nanty())
    }

    async fn person(&self, channel: Channel, text: &str) -> ConversationMessage {
        let secrets = self.state.secret_set().await;
        let cfg = self.state.config().read().await.nanty.clone();
        person_message(&self.nanty(), &cfg, &secrets, channel, text).expect("mensagem")
    }

    async fn say(&self, text: &str) -> (ConversationMessage, ConversationMessage) {
        let person = self.person(Channel::Web, text).await;
        let answer = reply(&self.state, &person).await.expect("resposta");
        (person, answer)
    }

    /// O prompt de sistema do pedido `n` ao modelo.
    fn system(&self, n: usize) -> String {
        self.model.requests()[n].messages.iter().find(|m| m.role == Role::System).map(|m| m.content.clone()).unwrap_or_default()
    }

    async fn lang(&self) -> Lang {
        nanty_lang(&self.state.config().read().await.nanty, &self.nanty())
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn disk_signal() -> Signal {
    Signal::new(SignalKind::DiskLow, None).fact("min_gb", 5).fact("count", 1).extra("free_gb", 2)
}

#[tokio::test]
async fn the_conversation_answers_in_the_language_of_who_spoke_not_of_the_interface() {
    let model = Scripted::new(vec![say("A Loja está parada esperando você."), say("All good with the Loja.")]);
    let w = World::new("chat", "en", Arc::clone(&model), |_| {});
    // Português com a interface em inglês.
    let (person, answer) = w.say("Como está a Loja?").await;
    assert_eq!(person.lang.as_deref(), Some("pt-BR"), "a mensagem da pessoa grava o idioma");
    assert_eq!(answer.lang.as_deref(), Some("pt-BR"));
    let system = w.system(0);
    assert!(system.contains("Responda no idioma em que ela escreveu a `message`"), "{system}");
    assert!(system.contains("use português do Brasil (pt-BR)"), "a reserva do prompt é a da pessoa: {system}");
    // Inglês logo depois: a resposta troca junto.
    let (person, answer) = w.say("How is it going now?").await;
    assert_eq!((person.lang.as_deref(), answer.lang.as_deref()), (Some("en"), Some("en")));
    assert!(w.system(1).contains("use inglês (en)"));
    // A interface do resto do Nants não muda: o idioma lembrado e o dos projetos seguem os dela.
    assert_eq!(w.state.config().read().await.nanty.lang.as_deref(), Some("en"));
    assert_eq!(w.state.project_lang("p1").get(), Lang::PtBr, "o idioma dos projetos é só do cabeçalho");
}

#[tokio::test]
async fn the_reserve_without_a_model_detects_the_language_in_pt_and_en() {
    let w = World::new("reserve", "en", Scripted::new(vec![]), |c| c.nanty.connection_id.clear());
    let (_, pt) = w.say("e a Loja agora?").await;
    assert_eq!((pt.code.as_deref(), pt.lang.as_deref()), (Some("nanty.chat.reserve_status"), Some("pt-BR")));
    let text = render::message_text(Lang::En, &pt);
    assert_eq!(text, render::code_text(Lang::PtBr, "nanty.chat.reserve_status", &pt.params) + &lines(Lang::PtBr, &pt), "o fixo sai no idioma da mensagem");
    // "ok" não diz o idioma: vale o último da pessoa (pt), não a interface (en).
    let (person, ok) = w.say("ok").await;
    assert_eq!((person.lang.as_deref(), ok.lang.as_deref()), (Some("pt-BR"), Some("pt-BR")));
    let (_, en) = w.say("what about the Loja now?").await;
    assert_eq!(en.lang.as_deref(), Some("en"));
    assert!(render::message_text(Lang::PtBr, &en).starts_with(&render::code_text(Lang::En, "nanty.chat.reserve_status", &en.params)));
    let (_, help) = w.say("/help").await;
    assert_eq!((help.lang.as_deref(), help.params["commands"].as_str()), (Some("en"), Some("status, summary, pause, quiet, help")));
    // Toda mensagem leva o idioma, inclusive as fixas.
    assert!(w.nanty().conversation().iter().all(|m| m.lang.is_some()), "{:?}", w.nanty().conversation());
    assert!(w.model.requests().is_empty(), "a reserva não chama o modelo");
}

/// As linhas extras (`params.lines`) de uma mensagem, cada uma numa linha, no idioma.
fn lines(lang: Lang, message: &ConversationMessage) -> String {
    let Some(list) = message.params.get("lines").and_then(Value::as_array) else { return String::new() };
    list.iter()
        .filter_map(|l| {
            let code = l.get("code").and_then(Value::as_str)?;
            let params: Map<String, Value> = l.get("params").and_then(Value::as_object).cloned().unwrap_or_default();
            Some(format!("\n{}", render::code_text(lang, code, &params)))
        })
        .collect()
}

#[tokio::test]
async fn the_proactive_notice_follows_the_conversation_and_the_chosen_language_wins() {
    let notice = r#"{"summary": "Pouco espaço no disco.", "actions": []}"#;
    let w = World::new("notice", "en", Scripted::new(vec![say(notice)]), |c| c.nanty.connection_id.clear());
    assert_eq!(w.lang().await, Lang::En, "sem histórico: o idioma da interface");
    w.say("oi, tudo bem?").await;
    assert_eq!(w.lang().await, Lang::PtBr, "seguir a conversa");
    // Com o modelo de volta, o aviso proativo sai no idioma da conversa.
    w.state.config().write().await.nanty.connection_id = "main".into();
    let cfg = w.state.config().read().await.nanty.clone();
    w.nanty().observe(&cfg, &[disk_signal()], &[]);
    assert!(matches!(turn::proactive(&w.state, &w.nanty()).await, Some(turn::TurnOutcome::Reply(_))));
    assert!(w.system(0).contains("Escreva o `summary` em português do Brasil (pt-BR)"), "{}", w.system(0));
    let last = w.nanty().conversation().into_iter().rev().find(|m| m.from == Speaker::Nanty).expect("aviso");
    assert_eq!((last.text.as_deref(), last.lang.as_deref()), (Some("Pouco espaço no disco."), Some("pt-BR")));
    // O idioma escolhido nos Ajustes vence a conversa; a conversa continua no idioma de quem falou.
    w.state.config().write().await.nanty.language = NantyLanguage::En;
    assert_eq!(w.lang().await, Lang::En);
    w.state.config().write().await.nanty.connection_id.clear();
    let (_, answer) = w.say("e agora?").await;
    assert_eq!(answer.lang.as_deref(), Some("pt-BR"));
}

#[tokio::test]
async fn the_telegram_language_code_is_the_third_clue() {
    let w = World::new("code", "pt-BR", Scripted::new(vec![]), |c| c.nanty.connection_id.clear());
    w.nanty().note_telegram_lang(Some("en-US"));
    w.nanty().note_telegram_lang(Some("fr"));
    assert_eq!(w.nanty().snapshot().telegram_lang.as_deref(), Some("en"), "idioma fora de pt e en não conta");
    let person = w.person(Channel::Telegram, "ok").await;
    assert_eq!(person.lang.as_deref(), Some("en"));
    assert_eq!(w.nanty().snapshot().person_lang.as_deref(), Some("en"), "o language_code é um sinal da pessoa");
    let web = w.person(Channel::Web, "14").await;
    assert_eq!(web.lang.as_deref(), Some("en"), "o último idioma da pessoa");
}

#[tokio::test]
async fn the_quote_is_cut_in_the_message_and_in_the_memory() {
    let model = Scripted::new(vec![say("Certo."), say("Certo de novo.")]);
    let w = World::new("quote", "pt-BR", Arc::clone(&model), |_| {});
    // Uma citação gravada antes do corte (maior que a memória aceita).
    let mut old = ConversationMessage::from_nanty("legado".into(), w.nanty().now().epoch_ms, Channel::Telegram, "x", Map::new());
    old.from = Speaker::Person;
    old.code = None;
    old.text = Some("olha isso".into());
    old.relayed = Some(Relayed { forwarded: false, third_party: true, quote: Some("q".repeat(5000)) });
    w.nanty().push_message(&old);
    let secrets = w.state.secret_set().await;
    let cfg = w.state.config().read().await.nanty.clone();
    let relayed = Relayed { forwarded: false, third_party: true, quote: Some("z".repeat(5000)) };
    let person = person_message_from(&w.nanty(), &cfg, &secrets, Channel::Telegram, Some("o que acha?"), false, None, Some(relayed))
        .expect("mensagem");
    let stored = person.relayed.as_ref().and_then(|r| r.quote.clone()).unwrap_or_default();
    assert_eq!(stored.chars().count(), MAX_QUOTE_CHARS, "a citação gravada tem o corte");
    reply(&w.state, &person).await.expect("resposta");
    let input = model.input(0);
    let count = |v: &Value, c: char| v.as_str().unwrap_or_default().chars().filter(|x| *x == c).count();
    assert_eq!(count(&input["quote"], 'z'), MAX_QUOTE_CHARS, "a citação da mensagem vai cortada");
    let memory = input["memory"].as_array().cloned().unwrap_or_default();
    let legacy = memory.iter().find(|m| m["text"] == "olha isso").expect("memória");
    assert_eq!(count(&legacy["quote"], 'q'), 1500, "a citação da memória tem o corte do texto");
}
