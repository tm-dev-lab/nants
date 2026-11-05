//! Testes do resumo da manhã natural (spec 141 T10, G8 e C-10), só com fatos montados aqui (nunca
//! modelo nem chave real), com 24 avisos guardados de dois projetos:
//!
//! - **com o modelo:** os avisos chegam a ele como dados agrupados por projeto, com o defeito do Nants
//!   primeiro, sem nome de projeto (só ids); a mensagem é o selo e o texto dele, sem linha fixa, e os
//!   dados ficam para a janela;
//! - **sem o modelo:** a reserva lista os 24, em ordem, com o projeto em cada linha, e a contagem bate
//!   com as linhas.

use serde_json::{json, Map, Value};

use super::digest::{self, DigestFacts, NantsFault};
use super::render;
use super::store::{ConversationMessage, DigestItem, NantyState};
use super::tools;
use crate::i18n::{tr, Lang};

/// Um instante de referência (2026-11-04, de madrugada no fuso da máquina ou perto disso).
const BASE_MS: u64 = 1_762_225_200_000;
const MINUTE: u64 = 60_000;

/// Um aviso guardado: o fato no minuto `event`, guardado `late` minutos depois.
fn item(project: &str, kind: &str, event: u64, late: u64, extra: Value) -> DigestItem {
    let at = BASE_MS + event * MINUTE;
    let mut params: Map<String, Value> = extra.as_object().cloned().unwrap_or_default();
    params.insert("since".into(), json!(tools::hhmm(at)));
    DigestItem { at_ms: at + late * MINUTE, project_id: Some(project.into()), group: format!("{project}:{kind}"), code: format!("nanty.why.{kind}"), params }
}

/// 24 avisos de dois projetos, guardados fora da ordem dos fatos: a Loja entregou 12 tarefas; o Mini
/// Street Fighter entregou 10 e parou duas vezes por um defeito do Nants. Devolve também a ordem
/// certa (pelos fatos): (projeto, hora).
fn facts() -> (DigestFacts, Vec<(String, String)>) {
    let mut items = Vec::new();
    for i in 0..24u64 {
        // A ordem de chegada embaralha a dos fatos: o atraso varia de 0 a 150 minutos.
        let event = i * 7;
        let late = (i * 37) % 6 * 30;
        let (project, kind, extra) = match i % 2 {
            0 => ("p1", "task_delivered", json!({})),
            _ if i == 11 || i == 23 => ("p2", "run_blocked", json!({"reason": "nants_environment"})),
            _ => ("p2", "task_delivered", json!({})),
        };
        items.push(item(project, kind, event, late, extra));
    }
    // A ordem certa é a dos fatos (a da criação acima), não a da chegada.
    let name = |p: &str| if p == "p1" { "Loja".to_string() } else { "Mini Street Fighter".to_string() };
    let order: Vec<(String, String)> =
        items.iter().map(|i| (name(i.project_id.as_deref().unwrap_or_default()), i.params["since"].as_str().unwrap_or_default().to_string())).collect();
    // O resumo guarda na ordem de chegada, que não é a dos fatos.
    items.sort_by_key(|i| i.at_ms);
    assert_ne!(items.iter().map(|i| i.params["since"].clone()).collect::<Vec<_>>(), order.iter().map(|(_, s)| json!(s)).collect::<Vec<_>>());
    let facts = DigestFacts {
        projects: 2,
        paused: 1,
        items,
        nants_faults: vec![NantsFault { project_id: "p2".into(), name: "Mini Street Fighter".into() }],
        names: [("p1".to_string(), "Loja".to_string()), ("p2".to_string(), "Mini Street Fighter".to_string())].into_iter().collect(),
        ..DigestFacts::default()
    };
    (facts, order)
}

/// Sem o modelo: a lista completa, em ordem, com o projeto em cada linha, e a contagem certa.
#[test]
fn without_the_model_the_24_held_notices_come_in_order_with_the_project_on_each_line() {
    let (facts, order) = facts();
    let message = digest::compose(&facts, &crate::config::NantyConfig::default(), "m".into(), BASE_MS).expect("resumo");
    for lang in [Lang::PtBr, Lang::En] {
        let shown = render::message_text(lang, &message);
        let lines: Vec<&str> = shown.lines().collect();
        let header = crate::i18n::tr_plural(lang, "nanty.digest.held", 24, &[]);
        let at = lines.iter().position(|l| *l == header).unwrap_or_else(|| panic!("cabeçalho com 24: {shown}"));
        let listed = &lines[at + 1..];
        assert_eq!(listed.len(), 24, "a contagem bate com as linhas: {shown}");
        for (line, (project, since)) in listed.iter().zip(&order) {
            assert!(line.starts_with(&format!("{project}: ")), "o projeto na linha: {line}");
            assert!(line.contains(since.as_str()), "em ordem: {line} sem {since}\n{shown}");
        }
    }
    // A linha usa o porquê do dicionário, com o projeto na frente.
    let pt = render::message_text(Lang::PtBr, &message);
    let first = tr(Lang::PtBr, "nanty.why.task_delivered", &[("since", &order[0].1)]);
    assert!(pt.contains(&tr(Lang::PtBr, "nanty.digest.item", &[("project", "Loja"), ("text", &first)])), "{pt}");
    // O defeito do Nants também é dito na reserva.
    assert!(pt.contains(&tr(Lang::PtBr, "nanty.digest.nants_fault", &[("project", "Mini Street Fighter")])), "{pt}");
}

/// Com o modelo: os 24 chegam como dados agrupados por projeto, com o defeito do Nants primeiro, sem o
/// nome dos projetos; a mensagem é o selo e o texto dele, sem linha fixa nenhuma.
#[test]
fn with_the_model_the_24_held_notices_arrive_grouped_and_no_fixed_line_goes_out() {
    let (facts, order) = facts();
    let input = digest::digest_input(&facts, &NantyState::default());
    let held = &input.held;
    assert_eq!(held.len(), 2, "{held:?}");
    // O projeto preso por defeito do Nants vem primeiro, marcado como problema do Nants.
    assert_eq!((held[0].project_id.as_deref(), held[0].nants_fault, held[0].needs_attention), (Some("p2"), true, true));
    assert_eq!((held[1].project_id.as_deref(), held[1].nants_fault), (Some("p1"), false));
    assert_eq!(held.iter().map(|g| g.notices.len()).sum::<usize>(), 24);
    assert_eq!(held[0].notices.iter().filter(|n| n.kind == "run_blocked").count(), 2);
    // Em ordem dentro de cada grupo.
    for group in held {
        let name = if group.project_id.as_deref() == Some("p1") { "Loja" } else { "Mini Street Fighter" };
        let want: Vec<&str> = order.iter().filter(|(p, _)| p == name).map(|(_, s)| s.as_str()).collect();
        let got: Vec<&str> = group.notices.iter().map(|n| n.at.as_str()).collect();
        assert_eq!(got, want, "{name}");
    }
    // O JSON do modelo: os grupos, só com códigos e ids (nada de nome de projeto), e nenhum aviso
    // guardado solto entre os sinais.
    let json = input.to_json();
    assert!(json.contains("\"held\"") && json.contains("\"nants_fault\":true"), "{json}");
    assert!(!json.contains("Loja") && !json.contains("Mini Street Fighter"), "{json}");
    assert!(input.signals.is_empty(), "{:?}", input.signals);
    assert_eq!(input.counts.get("digest_items"), Some(&24));

    // A mensagem: o selo e o texto do modelo; nenhuma frase fixa do porquê nem a contagem.
    let mut message = digest::compose(&facts, &crate::config::NantyConfig::default(), "m".into(), BASE_MS).expect("resumo");
    let model = "Bom dia. O Mini Street Fighter está parado por um defeito do Nants, e a Loja avançou bem.";
    digest::with_model_text(&mut message, &facts, model.to_string());
    for (lang, seal) in [(Lang::PtBr, "Resumo da manhã"), (Lang::En, "Morning summary")] {
        let shown = render::message_text(lang, &message);
        assert_eq!(shown, format!("{seal}\n{model}"));
        assert!(!shown.contains(&crate::i18n::tr_plural(lang, "nanty.digest.held", 24, &[])));
    }
    // Os dados ficam para a janela, agrupados e com o nome.
    let data = message.params[digest::HELD].as_array().cloned().unwrap_or_default();
    assert_eq!(data.len(), 2);
    assert_eq!((data[0]["name"].as_str(), data[0]["nants_fault"].as_bool()), (Some("Mini Street Fighter"), Some(true)));
    assert_eq!(data[1]["name"], "Loja");
    assert!(message.params.get(digest::WHY).is_none() && message.params.get("lines").is_none());
}

/// O prompt pede um texto natural e curto, a partir de `held`, sem lista mecânica.
#[test]
fn the_digest_prompt_asks_for_natural_text_from_the_held_data() {
    for lang in [Lang::PtBr, Lang::En] {
        let system = crate::agents::prompts::nanty_system(lang);
        assert!(system.contains("`held`") && system.contains("sem lista mecânica") && system.contains("nenhuma lista do servidor vai junto"), "{lang:?}");
    }
}

/// Revisão 1 (F-01): nenhum resultado de ferramenta diz ao modelo que o servidor avisa a pessoa de
/// algo, a não ser os efeitos que ainda levam o aviso por um cartão, um botão ou a linha mantida
/// ([`render::KEPT`]). Os outros dizem o que aconteceu e o que ela pode fazer, para o modelo contar.
#[test]
fn no_tool_result_promises_a_server_notice_that_no_longer_goes_out() {
    // O meio que ainda leva o aviso, por chave.
    const CARRIED: [(&str, &str); 6] = [
        ("nanty.tool.open_link", "o botão de acesso ao Nants vai na resposta"),
        ("nanty.tool.open_computer", "linha mantida em `KEPT` (render::KEPT): nanty.chat.open_computer"),
        ("nanty.tool.memory_proposed", "o cartão de confirmação da memória"),
        ("nanty.tool.card_created", "o cartão da ação, com o texto e os botões"),
        ("nanty.tool.silence_card", "o cartão de confirmação do silêncio, com a hora"),
        ("nanty.tool.silence_set", "o cartão de desligar o silêncio, com a hora"),
    ];
    let promises = [
        "o servidor já", "o servidor avisa", "o servidor conta", "o servidor explica", "já avisou", "não repita", "the server already", "the server tells",
        "the server explains", "already told", "do not repeat", "don't repeat",
    ];
    let mut wrong = Vec::new();
    for lang in [Lang::PtBr, Lang::En] {
        let dict = if lang == Lang::PtBr { crate::i18n::pt_br::MESSAGES } else { crate::i18n::en::MESSAGES };
        for (key, text) in dict.iter().filter(|(k, _)| k.starts_with("nanty.tool.")) {
            let lower = text.to_lowercase();
            if promises.iter().any(|p| lower.contains(p)) && !CARRIED.iter().any(|(k, _)| k == key) {
                wrong.push(format!("{lang:?} {key}: {text}"));
            }
        }
    }
    assert!(wrong.is_empty(), "promete aviso do servidor que não sai:\n{}", wrong.join("\n"));
    // Revisão 2: fonte única para o que fazer sem link. A linha mantida traz os passos; o resultado da
    // ferramenta não os repete, só diz que não deu e que a linha do servidor vai depois.
    for (lang, settings, authorized) in [(Lang::PtBr, "Ajustes", "aparelho autorizado"), (Lang::En, "Settings", "authorized device")] {
        let tool = tr(lang, "nanty.tool.open_computer", &[]);
        let line = tr(lang, "nanty.chat.open_computer", &[]);
        assert!(!tool.contains(settings) && !tool.contains(authorized), "{lang:?}: {tool}");
        assert!(line.contains(settings) && line.contains(authorized), "{lang:?}: {line}");
    }
    // Os outros três da revisão 1 dizem o que fazer, nos dois idiomas.
    for (key, pt, en) in [
        ("nanty.tool.open_here", "pergunte qual projeto", "ask which project"),
        ("nanty.tool.silence_off", "voltam a sair", "go out again"),
        ("nanty.tool.memory_forgotten", "aba Memória", "Memory tab"),
    ] {
        assert!(tr(Lang::PtBr, key, &[]).contains(pt) && tr(Lang::En, key, &[]).contains(en), "{key}");
    }
    // A linha mantida fecha o texto do modelo; a dos outros efeitos não.
    let mut m = super::store::ConversationMessage::from_nanty("m".into(), 1, super::store::Channel::Telegram, "nanty.chat.open_computer", Map::new());
    m.text = Some("Daqui não dá.".into());
    m.lang = Some("pt-BR".into());
    assert_eq!(render::message_text(Lang::PtBr, &m), format!("Daqui não dá.\n{}", tr(Lang::PtBr, "nanty.chat.open_computer", &[])));
    m.code = Some("nanty.chat.silence_off".into());
    assert_eq!(render::message_text(Lang::PtBr, &m), "Daqui não dá.");
    // Também como linha extra de outro efeito.
    m.code = Some("nanty.chat.silence_set".into());
    m.params.insert("until".into(), json!("10:00"));
    m.params.insert(digest::WHY.into(), json!([{ "code": "nanty.chat.open_computer", "params": {} }]));
    assert_eq!(render::message_text(Lang::En, &ConversationMessage { lang: None, ..m.clone() }).lines().count(), 2);
}
