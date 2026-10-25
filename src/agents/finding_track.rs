//! Apontamentos com identidade (spec 124 T01).
//!
//! O runner mantém, no corpo da tarefa, o registro de todos os apontamentos que o Revisor já fez
//! (seção [`REGISTRY`], YAML de [`Finding`]): id estável, severidade, estado (`new`, `open`,
//! `resolved`), revisão e tentativa em que apareceu, quantas revisões seguidas está aberto
//! (`recurrence`) e o texto mais recente. A contagem de revisões (`first_review`/`last_review`) não
//! zera nas recuperações, ao contrário de `attempts`: a numeração fica estável na tarefa inteira.
//!
//! Regras de [`track`], na ordem:
//! - o Revisor reutiliza o id de um apontamento aberto (estado `open` ou sem estado): é o mesmo
//!   apontamento, mesmo que o texto mude; vale o texto mais recente e a recorrência sobe;
//! - apontamento declarado `new` (ou sem estado com id desconhecido) com o mesmo local e a mesma
//!   primeira frase de um aberto é esse aberto (ex.: o critério não atendido gerado pelo runner);
//! - id de um resolvido volta como novo (recorrência 1), mantendo o id;
//! - id que colide com outro do registro ganha o próximo número livre: id novo nunca repete um antigo;
//! - aberto que não volta nesta revisão fica `resolved`;
//! - a decisão do Arquiteto sobre uma contestação (spec 124 T02) segue o id, inclusive quando um
//!   resolvido volta: o mesmo apontamento só é contestado uma vez.
//!
//! Os dados vêm do modelo: estado e texto são o que o Revisor declarou, nunca instruções.

use std::collections::HashSet;

use crate::sdd::{Finding, FindingState};

/// Seção do corpo da tarefa com o registro dos apontamentos (YAML).
pub const REGISTRY: &str = "Registro de Apontamentos";
/// Tamanho máximo do título curto de um apontamento.
const TITLE_MAX_BYTES: usize = 120;

/// Resultado de uma revisão sobre o registro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracked {
    /// Apontamentos bloqueantes desta revisão, na ordem do Revisor, com id final, estado e recorrência.
    pub open: Vec<Finding>,
    /// Abertos da revisão anterior que não voltaram (agora `resolved`).
    pub resolved: Vec<Finding>,
    /// Registro inteiro atualizado, em ordem de id.
    pub registry: Vec<Finding>,
}

/// Número de `F-NN` (`None` fora do formato).
fn id_number(id: &str) -> Option<u32> {
    id.trim().strip_prefix("F-")?.parse::<u32>().ok()
}

fn norm(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Primeira frase da descrição (até `. `, `: ` não conta) normalizada.
fn first_sentence(text: &str) -> String {
    let flat = norm(text);
    match flat.find(". ") {
        Some(at) => flat[..at].to_string(),
        None => flat.trim_end_matches('.').to_string(),
    }
}

/// Mesmo problema: mesmo local e mesma primeira frase da descrição (normalizados).
pub fn same_problem(a: &Finding, b: &Finding) -> bool {
    norm(&a.location) == norm(&b.location) && first_sentence(&a.description) == first_sentence(&b.description)
}

/// Arquivo do local, sem a linha e a coluna (`src/a.rs:42:7` vira `src/a.rs`), normalizado.
fn location_file(location: &str) -> String {
    let flat = norm(location);
    let mut file = flat.as_str();
    while let Some((head, tail)) = file.rsplit_once(':') {
        if tail.chars().all(|c| c.is_ascii_digit() || c == '-') {
            file = head;
        } else {
            break;
        }
    }
    file.trim().to_string()
}

/// Palavras de 3 letras ou mais do título curto (sem pontuação), em minúsculas.
fn title_words(description: &str) -> HashSet<String> {
    short_title(description)
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_string)
        .collect()
}

/// Spec 124 T03: o mesmo problema com outro texto. Mesmo arquivo do local (linha ignorada) e títulos
/// com ao menos metade das palavras em comum, ou o mesmo problema de [`same_problem`]. Pega o
/// resolvido que volta como `new` com id novo, que a recorrência pelo id não vê.
pub fn similar(a: &Finding, b: &Finding) -> bool {
    if same_problem(a, b) {
        return true;
    }
    let file = location_file(&a.location);
    if file.is_empty() || file != location_file(&b.location) {
        return false;
    }
    let (wa, wb) = (title_words(&a.description), title_words(&b.description));
    let smaller = wa.len().min(wb.len());
    smaller > 0 && wa.intersection(&wb).count() * 2 >= smaller
}

/// Abertos parecidos com um apontamento já resolvido (outro id): `(aberto, resolvido)`, na ordem do
/// registro. É o sinal de recorrência que o id sozinho não mostra.
pub fn returned_like_resolved(registry: &[Finding]) -> Vec<(Finding, Finding)> {
    let resolved: Vec<&Finding> = registry.iter().filter(|f| f.state == Some(FindingState::Resolved)).collect();
    registry
        .iter()
        .filter(|f| f.state.is_some_and(FindingState::is_pending))
        .filter_map(|open| {
            resolved
                .iter()
                .find(|r| r.id != open.id && r.arbitration.is_none() && similar(open, r))
                .map(|r| (open.clone(), (*r).clone()))
        })
        .collect()
}

/// Título curto de um apontamento: a primeira frase da descrição, cortada em [`TITLE_MAX_BYTES`].
pub fn short_title(description: &str) -> String {
    let flat = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence = match flat.find(". ") {
        Some(at) => &flat[..at],
        None => flat.trim_end_matches('.'),
    };
    if sentence.len() <= TITLE_MAX_BYTES {
        return sentence.to_string();
    }
    let mut end = TITLE_MAX_BYTES;
    while !sentence.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &sentence[..end])
}

/// Registro de partida: o gravado ou, em tarefa anterior à spec 124, os "Findings Abertos" como
/// abertos desde a revisão 1.
pub fn seed(registry: &[Finding], previous_open: &[Finding]) -> Vec<Finding> {
    if !registry.is_empty() || previous_open.is_empty() {
        return registry.to_vec();
    }
    previous_open
        .iter()
        .map(|f| Finding {
            state: Some(FindingState::Open),
            recurrence: f.recurrence.max(1),
            first_review: f.first_review.max(1),
            first_attempt: f.first_attempt,
            last_review: f.last_review.max(1),
            ..f.clone()
        })
        .collect()
}

/// Abertos do registro (novos ou ainda abertos).
pub fn pending(registry: &[Finding]) -> Vec<Finding> {
    registry.iter().filter(|f| f.state.is_some_and(FindingState::is_pending)).cloned().collect()
}

/// Ordem do briefing do Desenvolvedor: os abertos há mais revisões primeiro, depois os mais antigos
/// e, por fim, o id.
pub fn by_recurrence(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        b.recurrence
            .cmp(&a.recurrence)
            .then(a.first_review.cmp(&b.first_review))
            .then(id_number(&a.id).unwrap_or(u32::MAX).cmp(&id_number(&b.id).unwrap_or(u32::MAX)))
            .then(a.id.cmp(&b.id))
    });
}

/// Aplica a revisão (`current`, os bloqueantes depois da convergência) ao registro. `attempt` é a
/// tentativa que está sendo revisada; a revisão é a seguinte à maior `last_review` do registro.
pub fn track(registry: &[Finding], previous_open: &[Finding], current: Vec<Finding>, attempt: u32) -> Tracked {
    let mut reg = seed(registry, previous_open);
    let review = reg.iter().map(|f| f.last_review).max().unwrap_or(0) + 1;
    let mut next = reg
        .iter()
        .chain(current.iter())
        .filter_map(|f| id_number(&f.id))
        .max()
        .unwrap_or(0)
        + 1;
    let current_ids: HashSet<String> = current.iter().map(|f| f.id.trim().to_string()).collect();
    let mut claimed: HashSet<String> = HashSet::new();
    let mut open = Vec::with_capacity(current.len());
    for mut f in current {
        f.id = f.id.trim().to_string();
        let declared = f.state;
        let is_open = |e: &Finding| e.state.is_some_and(FindingState::is_pending);
        let by_id = reg.iter().position(|e| e.id == f.id).filter(|_| !claimed.contains(&f.id));
        let reuses_id = declared != Some(FindingState::New) && by_id.is_some_and(|i| is_open(&reg[i]));
        let target = if reuses_id {
            by_id
        } else {
            reg.iter()
                .position(|e| is_open(e) && !claimed.contains(&e.id) && same_problem(e, &f))
                .or_else(|| by_id.filter(|&i| declared != Some(FindingState::New) && !is_open(&reg[i])))
        };
        match target {
            Some(i) if is_open(&reg[i]) => {
                let prior = &reg[i];
                f.id = prior.id.clone();
                f.state = Some(FindingState::Open);
                f.recurrence = prior.recurrence.max(1) + 1;
                f.first_review = prior.first_review;
                f.first_attempt = prior.first_attempt;
                // Spec 124 T02: a decisão sobre a contestação acompanha o apontamento.
                f.arbitration = prior.arbitration.clone();
            }
            Some(i) => {
                // Resolvido que volta: mesmo id, conta de novo.
                f.id = reg[i].id.clone();
                f.arbitration = reg[i].arbitration.clone();
                f.state = Some(FindingState::New);
                f.recurrence = 1;
                f.first_review = review;
                f.first_attempt = attempt;
            }
            None => {
                let taken = claimed.contains(&f.id) || reg.iter().any(|e| e.id == f.id) || id_number(&f.id).is_none();
                if taken {
                    // Número livre que também não colide com outro id desta mesma revisão.
                    while current_ids.contains(&format!("F-{next:02}")) {
                        next += 1;
                    }
                    f.id = format!("F-{next:02}");
                    next += 1;
                }
                f.state = Some(FindingState::New);
                f.recurrence = 1;
                f.first_review = review;
                f.first_attempt = attempt;
            }
        }
        f.last_review = review;
        claimed.insert(f.id.clone());
        open.push(f);
    }
    let mut resolved = Vec::new();
    for e in reg.iter_mut() {
        if claimed.contains(&e.id) || !e.state.is_some_and(FindingState::is_pending) {
            continue;
        }
        e.state = Some(FindingState::Resolved);
        e.last_review = review;
        resolved.push(e.clone());
    }
    for f in &open {
        match reg.iter_mut().find(|e| e.id == f.id) {
            Some(e) => *e = f.clone(),
            None => reg.push(f.clone()),
        }
    }
    reg.sort_by(|a, b| id_number(&a.id).unwrap_or(u32::MAX).cmp(&id_number(&b.id).unwrap_or(u32::MAX)).then(a.id.cmp(&b.id)));
    Tracked { open, resolved, registry: reg }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &str, location: &str, description: &str) -> Finding {
        Finding {
            id: id.into(),
            severity: "HIGH".into(),
            location: location.into(),
            description: description.into(),
            recommendation: "corrigir".into(),
            ..Finding::default()
        }
    }

    fn with_state(mut finding: Finding, state: FindingState) -> Finding {
        finding.state = Some(state);
        finding
    }

    fn ids(list: &[Finding]) -> Vec<&str> {
        list.iter().map(|f| f.id.as_str()).collect()
    }

    /// Spec 124 T03 (nota da revisão da T01): o resolvido que volta declarado `new` ganha id novo;
    /// a semelhança de local e título o liga ao antigo.
    #[test]
    fn returned_finding_with_new_id_is_linked_to_the_resolved_one() {
        let old = with_state(f("F-02", "tests/e2e/two.spec.ts:40", "Convergência entre dois clientes não é testada. Detalhe."), FindingState::Resolved);
        let back = with_state(f("F-07", "tests/e2e/two.spec.ts:88", "Falta testar a convergência entre dois clientes"), FindingState::New);
        let other = with_state(f("F-08", "src/server/index.ts:3", "Convergência entre dois clientes não é testada"), FindingState::New);
        let pairs = returned_like_resolved(&[old.clone(), back.clone(), other]);
        assert_eq!(pairs.iter().map(|(a, b)| (a.id.as_str(), b.id.as_str())).collect::<Vec<_>>(), vec![("F-07", "F-02")]);
        assert!(similar(&back, &old));
        assert!(!similar(&back, &f("F-09", "tests/e2e/two.spec.ts:1", "Capturas fora do enquadramento")));
        // Dispensado pelo Arquiteto não conta como recorrência.
        let mut waived = old;
        waived.arbitration = Some(crate::sdd::Arbitration { decision: crate::sdd::ArbitrationDecision::Manual, reason: String::new(), review: 2 });
        assert!(returned_like_resolved(&[waived, back]).is_empty());
    }

    #[test]
    fn first_review_registers_everything_as_new() {
        let t = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X"), f("F-02", "src/b.rs:2", "falta Y")], 1);
        assert_eq!(ids(&t.open), vec!["F-01", "F-02"]);
        assert!(t.open.iter().all(|x| x.state == Some(FindingState::New) && x.recurrence == 1 && x.first_review == 1));
        assert!(t.open.iter().all(|x| x.first_attempt == 1 && x.last_review == 1));
        assert!(t.resolved.is_empty());
        assert_eq!(t.registry.len(), 2);
    }

    #[test]
    fn reused_id_keeps_identity_and_counts_recurrence_and_others_resolve() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X"), f("F-02", "src/b.rs:2", "falta Y")], 1);
        let r2 = track(&r1.registry, &r1.open, vec![with_state(f("F-01", "src/a.rs:9", "ainda falta X"), FindingState::Open)], 2);
        assert_eq!(ids(&r2.open), vec!["F-01"]);
        assert_eq!(r2.open[0].state, Some(FindingState::Open));
        assert_eq!(r2.open[0].recurrence, 2);
        assert_eq!((r2.open[0].first_review, r2.open[0].first_attempt, r2.open[0].last_review), (1, 1, 2));
        assert_eq!(ids(&r2.resolved), vec!["F-02"]);
        let r3 = track(&r2.registry, &pending(&r2.registry), vec![f("F-01", "src/a.rs:9", "ainda falta X")], 3);
        assert_eq!(r3.open[0].recurrence, 3, "sem estado declarado, o id reutilizado é o mesmo apontamento");
        let states: Vec<_> = r3.registry.iter().map(|x| (x.id.as_str(), x.state)).collect();
        assert_eq!(states, vec![("F-01", Some(FindingState::Open)), ("F-02", Some(FindingState::Resolved))]);
    }

    #[test]
    fn new_finding_never_reuses_an_old_id() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X")], 1);
        // Revisor que recomeça em F-01 declarando um problema novo: o runner dá o próximo número.
        let r2 = track(&r1.registry, &r1.open, vec![with_state(f("F-01", "src/c.rs:3", "outro problema"), FindingState::New)], 2);
        assert_eq!(ids(&r2.open), vec!["F-02"]);
        assert_eq!(ids(&r2.resolved), vec!["F-01"]);
        // F-02 declarado pelo Revisor e um novo colidindo: o novo pula o id que esta revisão já usa.
        let r3 = track(
            &r2.registry,
            &pending(&r2.registry),
            vec![with_state(f("F-01", "src/d.rs:1", "terceiro"), FindingState::New), with_state(f("F-03", "src/e.rs:1", "quarto"), FindingState::New)],
            1,
        );
        assert_eq!(ids(&r3.open), vec!["F-04", "F-03"]);
        assert_eq!(r3.registry.len(), 4);
    }

    #[test]
    fn same_text_under_new_id_is_the_open_finding() {
        let r1 = track(&[], &[], vec![f("F-01", "critérios de aceitação", "Critério não atendido: A. Evidência: nenhuma")], 1);
        let generated = with_state(f("F-01", "critérios de aceitação", "Critério não atendido: A. Evidência: outra"), FindingState::New);
        let r2 = track(&r1.registry, &r1.open, vec![generated], 2);
        assert_eq!(ids(&r2.open), vec!["F-01"]);
        assert_eq!(r2.open[0].recurrence, 2);
        assert!(r2.resolved.is_empty());
    }

    #[test]
    fn reused_id_for_a_different_problem_is_accepted_with_the_latest_text() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta validar a entrada")], 1);
        let other = with_state(f("F-01", "src/z.rs:7", "vazamento de memória no cache"), FindingState::Open);
        let r2 = track(&r1.registry, &r1.open, vec![other], 2);
        assert_eq!(r2.open[0].id, "F-01");
        assert_eq!(r2.open[0].recurrence, 2, "o runner aceita o que o Revisor declara");
        let entry = r2.registry.iter().find(|x| x.id == "F-01").expect("F-01");
        assert_eq!(entry.description, "vazamento de memória no cache");
        assert_eq!(entry.location, "src/z.rs:7");
    }

    #[test]
    fn resolved_finding_that_comes_back_restarts_count_with_same_id() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X")], 1);
        let r2 = track(&r1.registry, &r1.open, vec![f("F-02", "src/b.rs:1", "falta Y")], 2);
        let r3 = track(&r2.registry, &pending(&r2.registry), vec![f("F-01", "src/a.rs:1", "falta X")], 3);
        let back = r3.open.iter().find(|x| x.id == "F-01").expect("F-01");
        assert_eq!((back.state, back.recurrence, back.first_review), (Some(FindingState::New), 1, 3));
        assert_eq!(ids(&r3.resolved), vec!["F-02"]);
    }

    #[test]
    fn numbering_is_stable_across_recoveries_where_attempts_reset() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X")], 3);
        // Recuperação: attempts volta a 0; a revisão seguinte é a tentativa 1, mas a revisão 2.
        let r2 = track(&r1.registry, &r1.open, vec![f("F-01", "src/a.rs:1", "falta X"), with_state(f("F-07", "src/b.rs:1", "novo"), FindingState::New)], 1);
        assert_eq!(r2.open[0].recurrence, 2);
        assert_eq!((r2.open[0].first_review, r2.open[0].first_attempt), (1, 3));
        assert_eq!((r2.open[1].id.as_str(), r2.open[1].first_review, r2.open[1].first_attempt), ("F-07", 2, 1));
    }

    #[test]
    fn legacy_open_findings_seed_the_registry() {
        let legacy = vec![f("F-01", "src/a.rs:1", "falta X")];
        let t = track(&[], &legacy, vec![f("F-01", "src/a.rs:1", "falta X")], 2);
        assert_eq!((t.open[0].state, t.open[0].recurrence, t.open[0].last_review), (Some(FindingState::Open), 2, 2));
    }

    #[test]
    fn approval_resolves_all_open() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "x"), f("F-02", "src/b.rs:1", "y")], 1);
        let r2 = track(&r1.registry, &r1.open, vec![], 2);
        assert_eq!(ids(&r2.resolved), vec!["F-01", "F-02"]);
        assert!(pending(&r2.registry).is_empty());
    }

    #[test]
    fn briefing_order_puts_the_oldest_open_first() {
        let mut list = vec![
            Finding { id: "F-03".into(), recurrence: 1, first_review: 3, ..Finding::default() },
            Finding { id: "F-01".into(), recurrence: 3, first_review: 1, ..Finding::default() },
            Finding { id: "F-02".into(), recurrence: 2, first_review: 2, ..Finding::default() },
            Finding { id: "F-04".into(), recurrence: 1, first_review: 3, ..Finding::default() },
        ];
        by_recurrence(&mut list);
        assert_eq!(ids(&list), vec!["F-01", "F-02", "F-03", "F-04"]);
    }

    #[test]
    fn short_title_is_the_first_sentence_clipped() {
        assert_eq!(short_title("Falta validar. Detalhes longos aqui."), "Falta validar");
        assert_eq!(short_title("Sem ponto"), "Sem ponto");
        let long = "á".repeat(100);
        let t = short_title(&long);
        assert!(t.ends_with('…') && t.len() <= TITLE_MAX_BYTES + '…'.len_utf8());
    }

    #[test]
    fn registry_yaml_round_trips_and_old_findings_still_parse() {
        let t = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X")], 1);
        let yaml = serde_yaml::to_string(&t.registry).expect("yaml");
        assert!(yaml.contains("state: new") && yaml.contains("recurrence: 1") && yaml.contains("first_review: 1"));
        let back: Vec<Finding> = serde_yaml::from_str(&yaml).expect("parse");
        assert_eq!(back, t.registry);
        let old: Vec<Finding> = serde_yaml::from_str("- id: F-01\n  severity: HIGH\n  location: a\n  description: b\n  recommendation: c\n").expect("antigo");
        assert_eq!((old[0].state, old[0].recurrence), (None, 0));
    }
}
