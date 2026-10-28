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
//! - spec 131 T02: o apontamento com `continues: F-xx` (id que existe no registro, ver
//!   [`apply_continues`]) é esse apontamento, aberto ou resolvido: o id é reaberto e a recorrência sobe;
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

/// Arquivos do local, sem a linha e a coluna, normalizados (spec 131 T02). Usa a mesma separação de
/// [`crate::agents::runner::parse_locations`], então um local com vários trechos
/// (`a.py:46-48, a.py:150-176`) vira o conjunto `{a.py}`. Sem nenhum caminho analisável (ex.:
/// `critérios de aceitação`), o local inteiro normalizado é o único elemento; vazio, nenhum.
fn location_files(location: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for (path, _) in crate::agents::runner::parse_locations(location) {
        let path = path.trim().trim_start_matches("./").trim_start_matches('/').to_lowercase();
        if !path.is_empty() && !files.contains(&path) {
            files.push(path);
        }
    }
    if files.is_empty() {
        let flat = norm(location);
        if !flat.is_empty() {
            files.push(flat);
        }
    }
    files
}

/// Mesmo caminho, com tolerância a prefixo de pasta (`src/a.rs` e `a.rs` não; `x/src/a.rs` e `src/a.rs` sim).
fn same_path(a: &str, b: &str) -> bool {
    a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// Mesmo conjunto de arquivos: cada arquivo de um lado está no outro (spec 131 T02).
fn same_files(a: &[String], b: &[String]) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.iter().all(|x| b.iter().any(|y| same_path(x, y)))
        && b.iter().all(|y| a.iter().any(|x| same_path(x, y)))
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

/// Spec 124 T03: o mesmo problema com outro texto. Mesmos arquivos do local (linhas ignoradas;
/// spec 131 T02: o conjunto dos arquivos, com vários trechos no mesmo local) e títulos com ao menos
/// metade das palavras em comum, ou o mesmo problema de [`same_problem`]. Pega o resolvido que volta
/// como `new` com id novo, que a recorrência pelo id não vê. Não há ligação pelo critério: a
/// continuação com outro assunto vem declarada pelo Revisor ([`apply_continues`]).
pub fn similar(a: &Finding, b: &Finding) -> bool {
    if same_problem(a, b) {
        return true;
    }
    if !same_files(&location_files(&a.location), &location_files(&b.location)) {
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

/// Número da próxima revisão da tarefa: a seguinte à maior `last_review` do registro (1 sem
/// registro). É o mesmo número que [`track`] grava; não zera nas recuperações.
pub fn next_review(registry: &[Finding]) -> u32 {
    registry.iter().map(|f| f.last_review).max().unwrap_or(0) + 1
}

/// Spec 127 T02: o apontamento é novo para a tarefa, nada parecido no registro (aberto ou
/// resolvido). Não é novo: o id de um apontamento do registro reutilizado sem `"status": "new"`, ou um
/// apontamento parecido com um do registro ([`similar`]: mesmo arquivo, linha ignorada, e títulos
/// próximos), inclusive o resolvido que volta com id novo e outra redação. Na dúvida, não é novo.
/// Spec 131 T02: a continuação declarada (`continues` com um id do registro) também não é nova.
pub fn is_new(registry: &[Finding], f: &Finding) -> bool {
    let id = f.id.trim();
    let continues = f.continues.trim();
    !registry.iter().any(|e| {
        (f.state != Some(FindingState::New) && !id.is_empty() && e.id == id)
            || (!continues.is_empty() && e.id == continues)
            || similar(e, f)
    })
}

/// Spec 131 T02: aplica o `continues` que o Revisor declarou. Só é aceito quando o id existe no
/// registro (aberto ou resolvido, sem dispensa do Arquiteto) e ainda não foi tomado por outro apontamento desta revisão: o
/// apontamento passa a ter esse id e o estado `open`, e o [`track`] o reabre contando a
/// recorrência. Id desconhecido, fora do formato ou repetido é ignorado (o campo fica vazio e vale
/// o resto do que o Revisor declarou). A ligação é só a declarada: nada é ligado pelo critério.
pub fn apply_continues(registry: &[Finding], findings: Vec<Finding>) -> Vec<Finding> {
    let mut taken: HashSet<String> = HashSet::new();
    findings
        .into_iter()
        .map(|mut f| {
            let target = f.continues.trim().to_string();
            // Spec 131 T02 (F-02 da revisão): o id dispensado pelo Arquiteto não é alvo, senão o
            // defeito novo herdaria a dispensa e viraria observação.
            let waived = |e: &Finding| e.arbitration.as_ref().is_some_and(|a| a.decision.waives());
            let known = id_number(&target).is_some() && registry.iter().any(|e| e.id.trim() == target && !waived(e));
            if known && taken.insert(target.clone()) {
                f.id = target.clone();
                f.state = Some(FindingState::Open);
                f.continues = target;
            } else {
                f.continues.clear();
            }
            f
        })
        .collect()
}

/// Aplica a revisão (`current`, os bloqueantes depois da convergência) ao registro. `attempt` é a
/// tentativa que está sendo revisada; a revisão é a seguinte à maior `last_review` do registro.
pub fn track(registry: &[Finding], previous_open: &[Finding], current: Vec<Finding>, attempt: u32) -> Tracked {
    track_with_observations(registry, previous_open, current, &[], attempt)
}

/// Spec 126 T04: como [`track`], com as observações da mesma revisão (o que a convergência
/// rebaixou). O id de uma observação fica reservado: nenhum bloqueante desta revisão o recebe (nem
/// por reutilização, nem pela numeração do runner), então um id nunca aparece duas vezes na mesma
/// mensagem. Um aberto do registro rebaixado a observação fica `resolved` no registro (decisão da
/// 124/T01), mas não entra em [`Tracked::resolved`]: na mensagem ele aparece só como observação.
pub fn track_with_observations(
    registry: &[Finding],
    previous_open: &[Finding],
    current: Vec<Finding>,
    observations: &[Finding],
    attempt: u32,
) -> Tracked {
    let mut reg = seed(registry, previous_open);
    let review = next_review(&reg);
    let reserved: HashSet<String> =
        observations.iter().map(|f| f.id.trim().to_string()).filter(|id| !id.is_empty()).collect();
    let mut next = reg
        .iter()
        .chain(current.iter())
        .chain(observations.iter())
        .filter_map(|f| id_number(&f.id))
        .max()
        .unwrap_or(0)
        + 1;
    let current_ids: HashSet<String> =
        current.iter().map(|f| f.id.trim().to_string()).chain(reserved.iter().cloned()).collect();
    // Os ids das observações contam como já usados nesta revisão.
    let mut claimed: HashSet<String> = reserved.clone();
    let mut open = Vec::with_capacity(current.len());
    for mut f in current {
        f.id = f.id.trim().to_string();
        let declared = f.state;
        let is_open = |e: &Finding| e.state.is_some_and(FindingState::is_pending);
        let by_id = reg.iter().position(|e| e.id == f.id).filter(|_| !claimed.contains(&f.id));
        let reuses_id = declared != Some(FindingState::New) && by_id.is_some_and(|i| is_open(&reg[i]));
        // Spec 131 T02: a continuação declarada ([`apply_continues`]) segue o id, aberto ou resolvido.
        let continued = !f.continues.is_empty() && by_id.is_some_and(|i| reg[i].id == f.continues);
        f.continues.clear();
        let target = if reuses_id || continued {
            by_id
        } else {
            reg.iter()
                .position(|e| is_open(e) && !claimed.contains(&e.id) && same_problem(e, &f))
                .or_else(|| by_id.filter(|&i| declared != Some(FindingState::New) && !is_open(&reg[i])))
        };
        match target {
            Some(i) if is_open(&reg[i]) || continued => {
                // Spec 131 T02: o resolvido que o Revisor declara continuado é reaberto e conta a recorrência.
                let prior = &reg[i];
                f.id = prior.id.clone();
                f.state = Some(FindingState::Open);
                f.recurrence = prior.recurrence.max(1) + 1;
                f.first_review = prior.first_review;
                f.first_attempt = prior.first_attempt;
                // Spec 124 T02: a decisão sobre a contestação acompanha o apontamento.
                f.arbitration = prior.arbitration.clone();
                // Spec 126 T02: a contagem de "corrigido" sem efeito e o gatilho antecipado seguem o id.
                f.fixed_claims = prior.fixed_claims;
                f.early_review = prior.early_review;
            }
            Some(i) => {
                // Resolvido que volta: mesmo id, conta de novo.
                f.id = reg[i].id.clone();
                f.arbitration = reg[i].arbitration.clone();
                // Spec 126 T02: o "corrigido" que não durou continua contando quando o apontamento volta.
                f.early_review = reg[i].early_review;
                f.fixed_claims = reg[i].fixed_claims;
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
                f.fixed_claims = 0;
                f.early_review = 0;
            }
        }
        f.last_review = review;
        claimed.insert(f.id.clone());
        open.push(f);
    }
    let blocking: HashSet<&str> = open.iter().map(|f| f.id.as_str()).collect();
    let mut resolved = Vec::new();
    for e in reg.iter_mut() {
        if blocking.contains(e.id.as_str()) || !e.state.is_some_and(FindingState::is_pending) {
            continue;
        }
        e.state = Some(FindingState::Resolved);
        e.last_review = review;
        if !reserved.contains(&e.id) {
            resolved.push(e.clone());
        }
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

/// Spec 126 T02: soma uma revisão a `fixed_claims` de cada apontamento que continuou aberto (estado
/// `open`) depois de o Desenvolvedor respondê-lo como corrigido ou com teste (`claimed`, os ids dessas
/// respostas na tentativa revisada). Atualiza os abertos e o registro.
pub fn count_fixed_claims(tracked: &mut Tracked, claimed: &[String]) {
    for f in tracked.open.iter_mut() {
        if f.state == Some(FindingState::Open) && claimed.iter().any(|c| c == &f.id) {
            f.fixed_claims += 1;
            if let Some(e) = tracked.registry.iter_mut().find(|e| e.id == f.id) {
                e.fixed_claims = f.fixed_claims;
            }
        }
    }
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

    /// Spec 127 T02: novo é o que o registro nunca viu; id reutilizado ou o mesmo problema não é.
    #[test]
    fn is_new_ignores_reused_ids_and_same_problems() {
        let r1 = track(&[], &[], vec![f("F-01", "src/a.rs:1", "falta X")], 1);
        let r2 = track(&r1.registry, &r1.open, vec![f("F-02", "src/b.rs:2", "falta Y")], 2);
        assert_eq!(next_review(&[]), 1);
        assert_eq!(next_review(&r2.registry), 3);
        // F-01 está resolvido: reutilizar o id (sem "new") ou repetir o problema não é novo.
        assert!(!is_new(&r2.registry, &f("F-01", "src/z.rs:9", "outro texto")));
        assert!(!is_new(&r2.registry, &with_state(f("F-09", "src/a.rs:1", "falta X"), FindingState::New)));
        assert!(!is_new(&r2.registry, &with_state(f("F-02", "src/b.rs:2", "ainda falta Y"), FindingState::Open)));
        // Declarado novo com um id antigo, mas outro problema: é novo.
        assert!(is_new(&r2.registry, &with_state(f("F-01", "src/c.rs:3", "terceiro problema"), FindingState::New)));
        assert!(is_new(&r2.registry, &f("F-03", "src/c.rs:3", "terceiro problema")));
        // Resolvido que volta com "new", id novo, outra linha do mesmo arquivo e título reescrito: não é novo.
        let r1 = track(&[], &[], vec![f("F-01", "tests/api/plays.test.ts:65", "Falha de validação não confere o evento SSE. Detalhe.")], 1);
        let r2 = track(&r1.registry, &r1.open, vec![], 2);
        let back = with_state(f("F-07", "tests/api/plays.test.ts:120", "A validação que falha não confere o evento SSE"), FindingState::New);
        assert!(!is_new(&r2.registry, &back));
        // No mesmo arquivo, mas outro assunto: é novo.
        assert!(is_new(&r2.registry, &with_state(f("F-08", "tests/api/plays.test.ts:200", "Ordem do histórico sem desempate"), FindingState::New)));
    }

    const T9_F02: (&str, &str) = (
        "tests/test_reporting.py:20-89",
        "The tests do not prove CSV and Markdown agree on ranked transaction IDs or uncategorized IDs. They also do not check the expected category values.",
    );
    const T9_F03_LOCATION: &str = "tests/test_reporting.py:46-48, tests/test_reporting.py:150-176";
    const T9_F03: &str = "The renderer test inserts a description that is already normalized, so it does not verify behavior for descriptions requiring whitespace normalization. The IDs are compared to each other only.";

    /// Spec 131 T02 (Test 9, T05): o local com vários trechos vira o conjunto dos arquivos, igual ao
    /// do F-02. Antes, `location_file` devolvia "tests/test_reporting.py:46-48, tests/test_reporting.py".
    #[test]
    fn several_locations_become_the_set_of_files() {
        assert_eq!(location_files(T9_F03_LOCATION), vec!["tests/test_reporting.py".to_string()]);
        assert!(same_files(&location_files(T9_F03_LOCATION), &location_files(T9_F02.0)));
        assert_eq!(location_files("`src/a.rs:5:3`, web/b.tsx"), vec!["src/a.rs".to_string(), "web/b.tsx".to_string()]);
        assert_eq!(location_files("Critérios de aceitação"), vec!["critérios de aceitação".to_string()]);
        assert!(!same_files(&location_files("src/a.rs:1, src/b.rs:2"), &location_files("src/a.rs:9")));
        // O F-02 reescrito nos trechos do F-03 casa com o F-02 (com o local antigo, não casava).
        let f02 = with_state(f("F-02", T9_F02.0, T9_F02.1), FindingState::Resolved);
        let rewritten = with_state(f("F-03", T9_F03_LOCATION, "The tests do not prove CSV and Markdown agree on ranked IDs"), FindingState::New);
        assert!(similar(&rewritten, &f02));
        assert!(!is_new(&[f02.clone()], &rewritten));
        // O F-03 real fala de outro assunto: sem ligação automática (nem pelo critério); só o `continues`.
        let real = with_state(f("F-03", T9_F03_LOCATION, T9_F03), FindingState::New);
        assert!(!similar(&real, &f02));
        assert!(is_new(&[f02.clone()], &real));
        let declared = Finding { continues: "F-02".into(), ..real };
        assert!(!is_new(&[f02], &declared));
    }

    /// Spec 131 T02: `continues` aceito só com um id do registro; reabre o resolvido contando a
    /// recorrência e segue o aberto como o mesmo apontamento.
    #[test]
    fn continues_reopens_the_declared_id_with_recurrence() {
        let r1 = track(&[], &[], vec![f("F-01", "tests/test_reporting.py:49-89", "Tie order"), f("F-02", T9_F02.0, T9_F02.1)], 1);
        // Revisão 2 da T05: o F-03 continua o F-02, ainda aberto.
        let f03 = Finding { continues: "F-02".into(), ..with_state(f("F-03", T9_F03_LOCATION, T9_F03), FindingState::New) };
        let current = apply_continues(&r1.registry, vec![f03]);
        assert_eq!((current[0].id.as_str(), current[0].state), ("F-02", Some(FindingState::Open)));
        let r2 = track(&r1.registry, &r1.open, current, 2);
        assert_eq!(ids(&r2.open), vec!["F-02"]);
        assert_eq!((r2.open[0].recurrence, r2.open[0].first_review, r2.open[0].description.as_str()), (2, 1, T9_F03));
        assert_eq!(ids(&r2.resolved), vec!["F-01"]);
        assert!(r2.registry.iter().all(|e| e.continues.is_empty()), "o registro não grava o campo");
        // Resolvido continuado: reabre com o mesmo id e a recorrência conta.
        let r3 = track(&r2.registry, &pending(&r2.registry), vec![], 3);
        let back = Finding { continues: "F-02".into(), ..with_state(f("F-09", "tests/x.py:1", "Outro texto"), FindingState::New) };
        let r4 = track(&r3.registry, &[], apply_continues(&r3.registry, vec![back]), 4);
        assert_eq!((r4.open[0].id.as_str(), r4.open[0].state, r4.open[0].recurrence), ("F-02", Some(FindingState::Open), 3));
        assert_eq!(r4.open[0].first_review, 1);
        // Id fora do registro, fora do formato ou repetido na mesma revisão: ignorado.
        let unknown = Finding { continues: "F-77".into(), ..with_state(f("F-09", "src/z.rs:1", "Novo"), FindingState::New) };
        let bad = Finding { continues: "F02".into(), ..with_state(f("F-10", "src/y.rs:1", "Outro"), FindingState::New) };
        let twice = Finding { continues: "F-01".into(), ..with_state(f("F-11", "src/w.rs:1", "Terceiro"), FindingState::New) };
        let first = Finding { continues: "F-01".into(), ..with_state(f("F-12", "src/v.rs:1", "Quarto"), FindingState::New) };
        let out = apply_continues(&r4.registry, vec![unknown, bad, first, twice]);
        assert_eq!(out.iter().map(|x| (x.id.as_str(), x.continues.as_str())).collect::<Vec<_>>(), vec![("F-09", ""), ("F-10", ""), ("F-01", "F-01"), ("F-11", "")]);
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

    /// Spec 126 T02: só conta o "corrigido" que continuou aberto, e a contagem segue o id quando o
    /// apontamento volta depois de resolvido.
    #[test]
    fn fixed_claims_count_only_what_stayed_open() {
        let first = track(&[], &[], vec![f("F-01", "src/a.rs:1", "Falta X"), f("F-02", "src/b.rs:1", "Falta Y")], 1);
        let mut second = track(&first.registry, &first.open, vec![f("F-01", "src/a.rs:1", "Falta X"), f("F-02", "src/b.rs:1", "Falta Y")], 2);
        count_fixed_claims(&mut second, &["F-01".to_string()]);
        let claims = |t: &Tracked, id: &str| t.registry.iter().find(|e| e.id == id).map(|e| e.fixed_claims);
        assert_eq!((claims(&second, "F-01"), claims(&second, "F-02")), (Some(1), Some(0)));
        let mut third = track(&second.registry, &second.open, vec![f("F-02", "src/b.rs:1", "Falta Y")], 3);
        count_fixed_claims(&mut third, &["F-01".to_string()]);
        assert_eq!(claims(&third, "F-01"), Some(1), "resolvido: não conta");
        let back = track(&third.registry, &third.open, vec![f("F-01", "src/a.rs:1", "Falta X")], 4);
        assert_eq!(back.open[0].fixed_claims, 1, "volta com a contagem");
    }
}
