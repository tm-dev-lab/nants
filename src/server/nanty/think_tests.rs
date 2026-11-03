//! Testes do `deep_think` (spec 140 T13, C-25 e C-19), só com o modelo simulado (nunca chave real): o
//! esforço maior no pedido seguinte, com o mesmo modelo, a mesma conexão e as leituras já feitas; uma
//! vez por mensagem; as recusas (máximo, adaptador que recua, teto da mensagem e orçamento do dia, em
//! tokens e em US$), sempre com o turno seguindo; a procedência que a rodada nova não lava; a rodada
//! maior no orçamento da conversa; e as travas do prompt do parceiro.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{json, Value};

use super::*;
use crate::agents::chat::ChatBackend;
use crate::agents::llm_client::{ChatRequest, ChatResponse, Role, ToolCall};
use crate::agents::LlmError;
use crate::config::{PriceSource, Pricing};
use crate::i18n::tr;
use crate::server::nanty::chat::tests::{base_config, write_project};
use crate::server::nanty::chat::{person_message, reply};
use crate::server::nanty::store::{Channel, ConversationMessage};
use crate::server::nanty::{tools, InstanceStatus};
use crate::server::state::AppState;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Uma resposta roteirizada: o texto, as chamadas e se o adaptador recuou do esforço.
#[derive(Clone, Default)]
struct Step {
    text: String,
    calls: Vec<ToolCall>,
    fell_back: bool,
}

fn say(text: &str) -> Step {
    Step { text: text.into(), ..Step::default() }
}

fn call(name: &str, args: Value) -> Step {
    Step::default().and(name, args)
}

impl Step {
    fn and(mut self, name: &str, args: Value) -> Self {
        let id = format!("c{}-{name}", self.calls.len());
        self.calls.push(ToolCall { id, name: name.into(), arguments: args.to_string() });
        self
    }

    fn fell_back(mut self) -> Self {
        self.fell_back = true;
        self
    }
}

/// O modelo simulado: responde o roteiro e anota cada pedido.
#[derive(Default)]
struct Fake {
    steps: Mutex<Vec<Step>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl Fake {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self { steps: Mutex::new(steps.into_iter().rev().collect()), ..Self::default() })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        lock(&self.requests).clone()
    }

    fn efforts(&self) -> Vec<Option<Effort>> {
        self.requests().iter().map(|r| r.effort).collect()
    }

    /// Os resultados de ferramenta do pedido `n`.
    fn results(&self, n: usize) -> Vec<String> {
        self.requests()[n].messages.iter().filter(|m| m.role == Role::Tool).map(|m| m.content.clone()).collect()
    }
}

impl ChatBackend for Fake {
    fn complete<'a>(&'a self, req: &'a ChatRequest) -> Pin<Box<dyn Future<Output = Result<ChatResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let served = {
                let mut requests = lock(&self.requests);
                requests.push(req.clone());
                requests.len()
            };
            let Some(mut step) = lock(&self.steps).pop() else {
                return Err(LlmError::Transport("sem resposta roteirizada".into()));
            };
            for c in &mut step.calls {
                c.id = format!("{}@{served}", c.id);
            }
            let mut message = ChatMessage::text(Role::Assistant, step.text);
            message.tool_calls = step.calls;
            Ok(ChatResponse {
                message,
                finish_reason: Some("stop".into()),
                usage: Some(Usage::new(500, 50)),
                fallback_used: step.fell_back,
                web_search_calls: 0,
                citations: Vec::new(),
                web_search_rejected: false,
                billing: Billing::Metered,
            })
        })
    }
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nants-nanty-140t13-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("dir");
    d
}

struct World {
    dir: PathBuf,
    state: AppState,
    model: Arc<Fake>,
}

impl World {
    fn new(tag: &str, steps: Vec<Step>, edit: impl FnOnce(&mut Config)) -> Self {
        let dir = temp(tag);
        let mut cfg = base_config(&dir, &[("p1", "Loja"), ("p2", "Blog")]);
        // Um orçamento folgado por padrão: cada teste aperta o que quer medir.
        cfg.nanty.daily_budget_tokens = 10_000_000;
        edit(&mut cfg);
        let state = AppState::new(dir.join("config.toml"), cfg, 3000)
            .with_data_dir(dir.join("data"))
            .with_nanty_clock(crate::server::nanty::clock::test_clock());
        assert_eq!(state.nanty().acquire(), InstanceStatus::Active);
        let model = Fake::new(steps);
        state.nanty().set_backend(Arc::clone(&model) as Arc<dyn ChatBackend>);
        write_project(&dir.join("p1"), "Vitrine nova", "nada", None);
        write_project(&dir.join("p2"), "Posts", "nada", None);
        Self { dir, state, model }
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

    /// O `deep_think` gravado em cada turno da conversa, na ordem.
    fn outcomes(&self) -> Vec<Option<String>> {
        self.nanty()
            .journal_tail(1000)
            .into_iter()
            .filter(|e| e.code.starts_with("turn.") && e.params.get("turn") == Some(&json!("conversation")))
            .map(|e| e.params.get("deep_think").and_then(Value::as_str).map(str::to_string))
            .collect()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.state.nanty().release();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn result(code: &str) -> String {
    tr(Lang::PtBr, &format!("nanty.tool.deep_think.{code}"), &[])
}

fn raised(level: &str) -> String {
    tr(Lang::PtBr, "nanty.tool.deep_think.raised", &[("effort", level)])
}

// ---- O nível ----

#[test]
fn the_effort_goes_one_level_up_with_the_ceiling_at_high() {
    assert_eq!(raise(Effort::Low), Some(Effort::Medium));
    assert_eq!(raise(Effort::Medium), Some(Effort::High));
    assert_eq!(raise(Effort::High), None);
    assert!(reserve(Effort::Low) < reserve(Effort::Medium) && reserve(Effort::Medium) < reserve(Effort::High));
}

// ---- A rodada maior ----

/// O pedido aceito sobe o esforço das rodadas seguintes, no mesmo modelo, com a mesma chave de cache e
/// as leituras já feitas, sem ler de novo; o gasto entra no dia, e o registro diz o desfecho.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_raises_the_next_rounds_on_the_same_model_and_keeps_the_reads() {
    let w = World::new(
        "raised",
        vec![call("project_status", json!({ "project_id": "p1" })), call("deep_think", json!({})), say("Eu faria em duas etapas.")],
        |_| {},
    );
    let answer = w.say("Vale dividir a Loja em dois serviços?").await;
    assert_eq!(answer.text.as_deref(), Some("Eu faria em duas etapas."));
    let requests = w.model.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Low), Some(Effort::Medium)]);
    assert!(requests.iter().all(|r| r.model == "m-nanty"), "o mesmo modelo");
    assert!(requests.iter().all(|r| r.cache_key == requests[0].cache_key), "a mesma conexão e o mesmo prefixo");
    // A rodada maior vê a leitura feita antes dela, e nenhuma leitura se repete.
    let results = w.model.results(2);
    assert_eq!(results.len(), 2);
    assert!(results[0].contains("Loja"), "{results:?}");
    assert_eq!(results[1], raised("medium"));
    assert_eq!(requests[2].messages[..requests[1].messages.len()], requests[1].messages[..], "o turno continua das mesmas mensagens");
    // C-19: as três rodadas no gasto do dia, e o registro com o desfecho e o que a maior gastou.
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    assert_eq!((spend.input_tokens, spend.output_tokens), (1500, 150));
    assert_eq!(w.outcomes(), [Some("raised".to_string())]);
    let entry = w.nanty().journal_tail(10).into_iter().rfind(|e| e.code == "turn.ok").expect("turno");
    assert_eq!(entry.params["deep_tokens"], json!(550));
}

/// Uma vez por mensagem da pessoa: o segundo pedido, na mesma resposta ou numa rodada seguinte, é
/// recusado e o esforço não sobe de novo. A mensagem seguinte dela pode pedir outra vez.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_happens_at_most_once_per_message_of_the_person() {
    let w = World::new(
        "once",
        vec![
            call("deep_think", json!({})).and("deep_think", json!({})),
            call("deep_think", json!({})),
            say("Pensei."),
            call("deep_think", json!({})),
            say("De novo."),
        ],
        |_| {},
    );
    assert_eq!(w.say("Qual o melhor caminho para o roadmap?").await.text.as_deref(), Some("Pensei."));
    assert_eq!(w.model.results(1), [raised("medium"), result("repeated")]);
    assert_eq!(w.model.results(2)[2], result("repeated"));
    assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Medium), Some(Effort::Medium)], "nunca dois níveis acima");
    // A próxima mensagem dela é outra sessão.
    assert_eq!(w.say("E para a arquitetura?").await.text.as_deref(), Some("De novo."));
    assert_eq!(w.model.results(4), [raised("medium")], "só os resultados desta mensagem");
    assert_eq!(w.model.efforts()[3..], [Some(Effort::Low), Some(Effort::Medium)]);
    assert_eq!(w.outcomes(), [Some("raised".to_string()), Some("raised".to_string())]);
}

// ---- As recusas: o turno segue no esforço normal ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_at_the_highest_effort_is_refused_and_the_turn_goes_on() {
    let w = World::new("max", vec![call("deep_think", json!({})), say("No mesmo nível.")], |c| c.nanty.effort = Effort::High);
    assert_eq!(w.say("Pensa bem nisso.").await.text.as_deref(), Some("No mesmo nível."));
    assert_eq!(w.model.results(1), [result("max")]);
    assert_eq!(w.model.efforts(), [Some(Effort::High), Some(Effort::High)]);
    assert_eq!(w.outcomes(), [Some("max".to_string())]);
}

/// O adaptador que já recuou do esforço nesta mensagem: recusa antes da rodada. O que aceitou o nível
/// normal e recuou do maior: o turno volta ao normal, e o processo lembra para a próxima mensagem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_with_an_adapter_that_does_not_take_the_effort_is_refused() {
    let w = World::new("unsupported", vec![call("deep_think", json!({})).fell_back(), say("Sem esforço.")], |_| {});
    assert_eq!(w.say("Pensa bem nisso.").await.text.as_deref(), Some("Sem esforço."));
    assert_eq!(w.model.results(1), [result("unsupported")]);
    assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Low)]);
    assert_eq!(w.outcomes(), [Some("unsupported".to_string())]);

    let w = World::new(
        "rejected",
        vec![
            call("deep_think", json!({})),
            call("project_status", json!({ "project_id": "p1" })).fell_back(),
            say("Respondi no normal."),
            call("deep_think", json!({})),
            say("Sem subir."),
        ],
        |_| {},
    );
    assert_eq!(w.say("Pensa bem nisso.").await.text.as_deref(), Some("Respondi no normal."));
    assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Medium), Some(Effort::Low)], "de volta ao normal");
    assert_eq!(w.say("E agora?").await.text.as_deref(), Some("Sem subir."));
    assert_eq!(w.model.results(4), [result("unsupported")]);
    assert_eq!(w.model.efforts()[3..], [Some(Effort::Low), Some(Effort::Low)]);
    assert_eq!(w.outcomes(), [Some("rejected".to_string()), Some("unsupported".to_string())]);
}

/// Sem orçamento para a rodada maior: o teto de tokens da mensagem e o dia, em tokens e em US$. A
/// recusa tem código, nada sobe, e a resposta sai.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_without_budget_for_the_bigger_round_is_refused_and_the_turn_goes_on() {
    let steps = || vec![call("deep_think", json!({})), say("Com o que tenho.")];
    let cases: [(&str, &str, Box<dyn FnOnce(&mut Config)>); 3] = [
        ("message", "message", Box::new(|c: &mut Config| c.nanty.chat.max_tokens_per_message = 5_000)),
        // A rodada normal cabe nos 6 mil tokens do dia; a maior, com a saída reservada, não.
        ("tokens", "budget", Box::new(|c: &mut Config| c.nanty.daily_budget_tokens = 6_000)),
        (
            "usd",
            "budget",
            Box::new(|c: &mut Config| {
                c.pricing.insert("m-nanty".into(), price(1.0, 10.0));
                c.nanty.daily_budget_usd = 0.05;
            }),
        ),
    ];
    for (tag, code, edit) in cases {
        let w = World::new(tag, steps(), edit);
        assert_eq!(w.say("Pensa bem nisso.").await.text.as_deref(), Some("Com o que tenho."), "{tag}");
        assert_eq!(w.model.results(1), [result(code)], "{tag}");
        assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Low)], "{tag}");
        assert_eq!(w.outcomes(), [Some(code.to_string())], "{tag}");
    }
    // Com folga em US$, a mesma rodada entra, e o gasto dela conta no dia em US$.
    let w = World::new("usd-ok", steps(), |c| {
        c.pricing.insert("m-nanty".into(), price(1.0, 10.0));
        c.nanty.daily_budget_usd = 5.0;
    });
    w.say("Pensa bem nisso.").await;
    assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Medium)]);
    let spend = w.nanty().snapshot().spend_on(&w.nanty().now().day_key());
    let expected = 2.0 * (500.0 * 1.0 + 50.0 * 10.0) / 1_000_000.0;
    assert!((spend.usd - expected).abs() < 1e-9, "{}", spend.usd);
    assert_eq!(spend.analysis_usd, 0.0, "a conversa nunca entra na fatia da análise");
}

fn price(input: f64, output: f64) -> Pricing {
    Pricing {
        input_per_mtok: input,
        output_per_mtok: output,
        cached_input_per_mtok: None,
        cache_write_per_mtok: None,
        web_search_per_call: None,
        image_per_unit: None,
        source: PriceSource::Manual,
        estimated_at: None,
    }
}

// ---- Procedência e modo ----

/// A rodada maior não lava a procedência: a leitura de antes dela contamina a resposta, e o tom fica
/// no neutro.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_keeps_the_provenance_of_the_turn() {
    let w = World::new(
        "taint",
        vec![call("project_details", json!({ "project_id": "p1" })), call("deep_think", json!({})), say("[tone: upbeat]\nAcho que vale, mas é opinião minha.")],
        |_| {},
    );
    let answer = w.say("O que acha do plano da Loja?").await;
    assert_eq!(w.model.efforts().last(), Some(&Some(Effort::Medium)));
    assert_eq!(answer.text.as_deref(), Some("Acho que vale, mas é opinião minha."));
    let taint = answer.taint.expect("procedência gravada");
    assert!(taint.is_tainted(), "{taint:?}");
    assert!(taint.sources.iter().any(|s| s.project_id.as_deref() == Some("p1")), "{taint:?}");
}

/// No modo de análise a ferramenta não é oferecida, e o despachante recusa o nome sem mudar o esforço.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deep_think_is_not_offered_in_the_analysis_mode() {
    assert!(tools::Mode::Conversation.allows(DEEP_THINK));
    assert!(!tools::Mode::Analysis.allows(DEEP_THINK));
    let w = World::new("analysis", vec![call("deep_think", json!({})), say("Causa provável: teste.")], |_| {});
    let cfg = w.state.config().read().await.clone();
    let mut session = Session::analysis(Channel::Telegram, super::super::provenance::Taint::default(), Lang::PtBr);
    let context = vec![ChatMessage::text(Role::User, "{}")];
    let ended = super::super::agent::run(&w.state, &w.nanty(), &cfg, "sistema".into(), context, &mut session).await;
    assert!(matches!(ended, super::super::agent::Ended::Reply { .. }), "{ended:?}");
    let requests = w.model.requests();
    assert!(requests[0].tools.iter().all(|t| t.name != DEEP_THINK), "não oferecida");
    assert_eq!(w.model.results(1), [tr(Lang::PtBr, "nanty.tool.unknown_tool", &[("name", DEEP_THINK)])]);
    assert_eq!(w.model.efforts(), [Some(Effort::Low), Some(Effort::Low)]);
    assert_eq!(session.think.outcome, None);
}

// ---- O prompt do parceiro ----

/// C-26: o prompt separa fato de hipótese, com a marca no próprio texto, pede o que faria e por quê,
/// o risco e a discordância, e não traz frase pronta, nem de elogio.
#[test]
fn the_partner_prompt_separates_fact_from_hypothesis_without_ready_phrases() {
    for lang in [Lang::PtBr, Lang::En] {
        let prompt = crate::agents::prompts::nanty_chat_system(lang);
        for rule in [
            "Separe o que você sabe do que você supõe",
            "não invente nenhum deles",
            "é hipótese",
            "o próprio texto deixe claro",
            "opinião ou suposição",
            "diga o que você faria e por quê",
            "aponte o risco",
            "discorde quando tiver razão",
            "sem abrir com elogio",
            "`deep_think`",
        ] {
            assert!(prompt.contains(rule), "{lang:?}: falta {rule}");
        }
        // A regra antiga, que proibia a hipótese, saiu.
        assert!(!prompt.contains("não ligue um fato a outro") && !prompt.contains("diga que não sabe"), "{lang:?}");
        // Nenhuma frase pronta: nada entre aspas, e nenhum elogio de abertura.
        let lower = prompt.to_lowercase();
        assert!(!prompt.contains(['"', '\u{201c}', '\u{201d}']), "{lang:?}: sem frase entre aspas");
        for praise in ["ótima pergunta", "boa pergunta", "excelente ideia", "ótima ideia", "boa ideia", "great question", "great idea"] {
            assert!(!lower.contains(praise), "{lang:?}: {praise}");
        }
    }
    let tool = crate::agents::prompts::nanty_chat_tools().into_iter().find(|t| t.name == DEEP_THINK).expect("ferramenta");
    assert_eq!(tool.parameters["properties"], json!({}), "sem configuração nem parâmetro");
}
