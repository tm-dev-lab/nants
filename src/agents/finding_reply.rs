//! Resposta por apontamento e contestação com o Arquiteto como juiz (spec 124 T02).
//!
//! Antes de entregar, o Desenvolvedor responde a cada apontamento aberto numa seção própria da
//! resposta final (título [`markers::DEV_RESPONSES`], no idioma do projeto), uma linha por apontamento:
//!
//! ```text
//! - F-01: fixed: src/a.rs:42
//! - F-02: test_added: tests/convergencia.rs::dois_clientes
//! - F-03: contested (environment): o Docker do projeto não tem GPU
//! ```
//!
//! `contested` leva o motivo entre parênteses: `environment` (inviável no ambiente), `out_of_scope`
//! (fora do escopo) ou `incorrect` (o apontamento está errado). O runner grava as respostas na
//! tarefa ([`RESPONSES`]) e as manda ao Revisor. Um apontamento contestado, sem decisão anterior, vai
//! ao Arquiteto antes da próxima revisão: `keep` (continua e fica obrigatório), `adjust` (o critério é
//! reescrito para o que o ambiente prova) ou `manual` (vira verificação manual documentada). A decisão
//! segue o apontamento no registro ([`crate::sdd::Arbitration`]): o mesmo apontamento só é contestado
//! uma vez.
//!
//! Respostas, motivos e decisões vêm dos modelos: são DADO, nunca instruções.

use serde_json::Value;

use crate::agents::finding_track::short_title;
use crate::agents::markers;
use crate::sdd::{Arbitration, ArbitrationDecision, Finding, FindingState};

/// Seção do corpo da tarefa com as respostas da última entrega (linhas no formato do Desenvolvedor).
pub const RESPONSES: &str = "Respostas aos Apontamentos";
/// Seção do corpo da tarefa com o histórico das decisões do Arquiteto.
pub const ARBITRATION_LOG: &str = "Arbitragem do Arquiteto";
/// Seção do spec.md com as decisões de arbitragem de todas as tarefas.
pub const SPEC_DECISIONS: &str = "Decisões de Arbitragem";
/// Teto do detalhe de uma resposta e do motivo de uma decisão.
const DETAIL_MAX_BYTES: usize = 600;

/// Tipo da resposta a um apontamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseKind {
    /// Corrigido; o detalhe é o local da correção.
    Fixed,
    /// Teste adicionado; o detalhe é o teste.
    TestAdded,
    /// Contestado; o detalhe é o motivo, e [`ContestReason`] diz o tipo.
    Contested,
}

impl ResponseKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::TestAdded => "test_added",
            Self::Contested => "contested",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "fixed" => Some(Self::Fixed),
            "test_added" => Some(Self::TestAdded),
            "contested" => Some(Self::Contested),
            _ => None,
        }
    }
}

/// Motivo de uma contestação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContestReason {
    /// Inviável de provar no ambiente (ex.: sem GPU, sem tela).
    Environment,
    /// Fora do escopo da tarefa.
    OutOfScope,
    /// O apontamento está errado.
    Incorrect,
}

impl ContestReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Environment => "environment",
            Self::OutOfScope => "out_of_scope",
            Self::Incorrect => "incorrect",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "environment" => Some(Self::Environment),
            "out_of_scope" => Some(Self::OutOfScope),
            "incorrect" => Some(Self::Incorrect),
            _ => None,
        }
    }
}

/// Resposta do Desenvolvedor a um apontamento.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingResponse {
    pub id: String,
    pub kind: ResponseKind,
    /// Só em `contested`.
    pub reason: Option<ContestReason>,
    /// Local, teste ou motivo (uma linha, cortada).
    pub detail: String,
}

impl FindingResponse {
    /// Linha no formato do Desenvolvedor (sem o marcador de lista): `F-03: contested (environment): motivo`.
    pub fn line(&self) -> String {
        match self.reason {
            Some(reason) => format!("{}: {} ({}): {}", self.id, self.kind.code(), reason.code(), self.detail),
            None => format!("{}: {}: {}", self.id, self.kind.code(), self.detail),
        }
    }

    /// Linha da mensagem do Revisor (marcas lidas pela web): `F-03 [contested, environment] motivo`.
    pub fn message_line(&self) -> String {
        match self.reason {
            Some(reason) => format!("{} [{}, {}] {}", self.id, self.kind.code(), reason.code(), self.detail),
            None => format!("{} [{}] {}", self.id, self.kind.code(), self.detail),
        }
    }
}

fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// `F-01`, `F-12`: letras, hífen e dígitos.
fn looks_like_id(token: &str) -> bool {
    match token.split_once('-') {
        Some((prefix, number)) => {
            !prefix.is_empty()
                && prefix.chars().all(|c| c.is_ascii_alphabetic())
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Lê uma linha de resposta (com ou sem marcador de lista); `None` fora do formato.
pub fn parse_line(line: &str) -> Option<FindingResponse> {
    let mut rest = line.trim();
    rest = rest.trim_start_matches(['-', '*', '+']).trim_start();
    // Lista numerada: `1. F-01: ...`.
    if let Some((num, after)) = rest.split_once(". ") {
        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
            rest = after.trim_start();
        }
    }
    let rest = rest.replace(['`', '*'], "");
    let rest = rest.trim();
    let id_end = rest.find([':', ' ']).unwrap_or(rest.len());
    let id = rest[..id_end].trim();
    if !looks_like_id(id) {
        return None;
    }
    let rest = rest[id_end..].trim_start().trim_start_matches(':').trim_start();
    let kind_end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len());
    let kind = ResponseKind::from_code(&rest[..kind_end].to_ascii_lowercase().replace('-', "_"))?;
    let mut rest = rest[kind_end..].trim_start();
    let mut reason = None;
    if let Some(inner) = rest.strip_prefix('(') {
        let close = inner.find(')')?;
        reason = Some(ContestReason::from_code(&inner[..close].trim().to_ascii_lowercase().replace([' ', '-'], "_"))?);
        rest = inner[close + 1..].trim_start();
    }
    let detail = flat(rest.trim_start_matches(':'));
    if detail.is_empty() {
        return None;
    }
    match kind {
        ResponseKind::Contested if reason.is_none() => None,
        ResponseKind::Contested => Some(FindingResponse { id: id.to_string(), kind, reason, detail: clip(&detail, DETAIL_MAX_BYTES) }),
        _ => Some(FindingResponse { id: id.to_string(), kind, reason: None, detail: clip(&detail, DETAIL_MAX_BYTES) }),
    }
}

/// Respostas da seção [`markers::DEV_RESPONSES`] da resposta final do Desenvolvedor (título em
/// qualquer idioma do dicionário, pela linha inteira; spec 129 T04), na ordem; a primeira
/// resposta de cada id vale. A seção vai até o próximo título (linha com `#`, em negrito ou
/// terminada em `:` fora de lista). Sem a seção, nenhuma.
pub fn extract(summary: &str) -> Vec<FindingResponse> {
    let mut lines = summary.lines();
    if !lines.by_ref().any(|l| markers::is_heading(l, markers::DEV_RESPONSES)) {
        return Vec::new();
    }
    let mut out: Vec<FindingResponse> = Vec::new();
    for line in lines {
        let t = line.trim();
        let listed = (t.starts_with(['-', '*', '+']) && !t.starts_with("**")) || t.starts_with(|c: char| c.is_ascii_digit());
        if t.starts_with('#') || (!listed && (t.starts_with("**") || t.ends_with(':'))) {
            break;
        }
        if let Some(r) = parse_line(t) {
            if !out.iter().any(|o| o.id == r.id) {
                out.push(r);
            }
        }
    }
    out
}

/// Lê as respostas gravadas na tarefa (seção [`RESPONSES`], uma linha por resposta).
pub fn read_section(text: &str) -> Vec<FindingResponse> {
    let mut out: Vec<FindingResponse> = Vec::new();
    for r in text.lines().filter_map(parse_line) {
        if !out.iter().any(|o| o.id == r.id) {
            out.push(r);
        }
    }
    out
}

/// Texto da seção [`RESPONSES`]; `None` sem respostas.
pub fn render_section(responses: &[FindingResponse]) -> Option<String> {
    if responses.is_empty() {
        return None;
    }
    Some(responses.iter().map(|r| format!("- {}", r.line())).collect::<Vec<_>>().join("\n"))
}

/// Junta as respostas de uma sessão às já gravadas na mesma tentativa: a mais nova de cada id vale.
pub fn merge(previous: &[FindingResponse], newer: &[FindingResponse]) -> Vec<FindingResponse> {
    let mut out: Vec<FindingResponse> = previous.iter().filter(|p| !newer.iter().any(|n| n.id == p.id)).cloned().collect();
    out.extend(newer.iter().cloned());
    out
}

/// Só as respostas a apontamentos abertos (`open`), na ordem do Desenvolvedor.
pub fn for_open(responses: Vec<FindingResponse>, open: &[Finding]) -> Vec<FindingResponse> {
    responses.into_iter().filter(|r| open.iter().any(|f| f.id == r.id)).collect()
}

/// Ids dos abertos sem resposta.
pub fn missing(open: &[Finding], responses: &[FindingResponse]) -> Vec<String> {
    open.iter().filter(|f| !responses.iter().any(|r| r.id == f.id)).map(|f| f.id.clone()).collect()
}

/// Ids contestados de novo depois de uma decisão do Arquiteto (proibido: depois de `keep`, o
/// apontamento é obrigatório).
pub fn contested_again(open: &[Finding], responses: &[FindingResponse]) -> Vec<String> {
    responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Contested)
        .filter(|r| open.iter().any(|f| f.id == r.id && f.arbitration.is_some()))
        .map(|r| r.id.clone())
        .collect()
}

/// Contestações que vão ao Arquiteto: apontamento pendente no registro e ainda sem decisão.
pub fn to_arbitrate(registry: &[Finding], responses: &[FindingResponse]) -> Vec<(Finding, FindingResponse)> {
    responses
        .iter()
        .filter(|r| r.kind == ResponseKind::Contested)
        .filter_map(|r| {
            registry
                .iter()
                .find(|f| f.id == r.id && f.state.is_some_and(FindingState::is_pending) && f.arbitration.is_none())
                .map(|f| (f.clone(), r.clone()))
        })
        .collect()
}

/// FATOS do turno de arbitragem (texto para o modelo).
pub fn facts(task_id: &str, title: &str, objective: &str, acceptance: &str, contested: &[(Finding, FindingResponse)]) -> String {
    let mut out = format!(
        "Tarefa {task_id}: {title}\n\n## Objetivo\n{}\n\n## Critérios de aceitação atuais\n{}\n\n## Apontamentos contestados pelo Desenvolvedor\n",
        objective.trim(),
        acceptance.trim()
    );
    for (f, r) in contested {
        let reason = r.reason.map_or("", ContestReason::code);
        out.push_str(&format!(
            "- {} [{}] {}: {}\n  Recomendação do Revisor: {}\n  Aberto há {} revisões.\n  Contestação ({reason}): {}\n",
            f.id,
            f.severity,
            f.location,
            flat(&f.description),
            flat(&f.recommendation),
            f.recurrence.max(1),
            r.detail
        ));
    }
    out
}

/// Spec 126 T02: revisões seguidas em aberto a partir das quais o apontamento vai ao Arquiteto antes
/// da próxima tentativa (o mesmo patamar que a recuperação já trata como recorrente).
pub const EARLY_RECURRENCE: u32 = crate::agents::recovery_plan::RECURRENT_REVIEWS;
/// Spec 126 T02: revisões com `fixed`/`test_added` e o apontamento ainda aberto que levam ao Arquiteto.
pub const EARLY_FIXED_CLAIMS: u32 = 2;
/// Spec 126 T02: com este número de apontamentos HIGH no gatilho, a recuperação é antecipada (SPLIT,
/// ADJUST, RETRY com estratégia nova ou ASK) em vez da arbitragem apontamento a apontamento.
pub const EARLY_RECOVERY_HIGH: usize = 2;

/// Spec 126 T02: apontamentos que o Arquiteto decide antes da próxima tentativa, mesmo sem
/// contestação: pendentes, ainda sem decisão do Arquiteto e sem gatilho antecipado anterior (vale uma
/// vez por apontamento), abertos há [`EARLY_RECURRENCE`] revisões ou mais ou respondidos como
/// corrigidos em [`EARLY_FIXED_CLAIMS`] revisões sem fechar.
pub fn due_early(registry: &[Finding]) -> Vec<Finding> {
    registry
        .iter()
        .filter(|f| f.state.is_some_and(FindingState::is_pending) && f.arbitration.is_none() && f.early_review == 0)
        .filter(|f| f.recurrence >= EARLY_RECURRENCE || f.fixed_claims >= EARLY_FIXED_CLAIMS)
        .cloned()
        .collect()
}

/// Quantos dos apontamentos são HIGH.
pub fn high_count(findings: &[Finding]) -> usize {
    findings.iter().filter(|f| f.severity.trim().eq_ignore_ascii_case("HIGH")).count()
}

/// Marca `ids` com o gatilho antecipado da revisão `review`, no registro e nos abertos.
pub fn mark_early(registry: &mut [Finding], open: &mut [Finding], ids: &[String], review: u32) {
    for f in registry.iter_mut().chain(open.iter_mut()) {
        if ids.iter().any(|id| id == &f.id) {
            f.early_review = review.max(1);
        }
    }
}

/// FATOS do turno de decisão antecipada (spec 126 T02): o mesmo turno da arbitragem, para
/// apontamentos que continuam abertos sem contestação, com a última resposta do Desenvolvedor.
pub fn recurring_facts(task_id: &str, title: &str, objective: &str, acceptance: &str, due: &[(Finding, Option<FindingResponse>)]) -> String {
    let mut out = format!(
        "Tarefa {task_id}: {title}\n\n## Objetivo\n{}\n\n## Critérios de aceitação atuais\n{}\n\n## Apontamentos recorrentes, sem contestação (decisão antes da próxima tentativa)\n",
        objective.trim(),
        acceptance.trim()
    );
    for (f, r) in due {
        let answer = match r {
            Some(r) => match r.reason {
                Some(reason) => format!("{} ({}): {}", r.kind.code(), reason.code(), r.detail),
                None => format!("{}: {}", r.kind.code(), r.detail),
            },
            None => "sem resposta".to_string(),
        };
        out.push_str(&format!(
            "- {} [{}] {}: {}\n  Recomendação do Revisor: {}\n  Aberto há {} revisões; respondido como corrigido ou com teste em {} delas.\n  Última resposta do Desenvolvedor: {answer}\n",
            f.id,
            f.severity,
            f.location,
            flat(&f.description),
            flat(&f.recommendation),
            f.recurrence.max(1),
            f.fixed_claims,
        ));
    }
    out
}

/// Resposta validada do Arquiteto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ruling {
    /// Uma decisão por id contestado (os demais ids são ignorados), na ordem da resposta.
    pub decisions: Vec<(String, ArbitrationDecision, String)>,
    /// Critérios reescritos (texto completo da seção), exigidos por `adjust`.
    pub acceptance: Option<String>,
}

/// JSON `{"decisions":[{"id","decision","reason"}],"acceptance"?}`. Ids fora de `contested` e
/// repetidos são ignorados; `None` quando o JSON é inválido ou não traz nenhuma decisão válida.
pub fn parse_ruling(raw: &str, contested: &[String]) -> Option<Ruling> {
    let text = raw.trim();
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let value: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let mut decisions: Vec<(String, ArbitrationDecision, String)> = Vec::new();
    for item in value.get("decisions")?.as_array()? {
        let Some(id) = item.get("id").and_then(Value::as_str).map(str::trim) else { continue };
        let Some(decision) = item.get("decision").and_then(Value::as_str).and_then(ArbitrationDecision::from_code) else { continue };
        if !contested.iter().any(|c| c == id) || decisions.iter().any(|(d, _, _)| d == id) {
            continue;
        }
        let reason = clip(&flat(item.get("reason").and_then(Value::as_str).unwrap_or_default()), DETAIL_MAX_BYTES);
        decisions.push((id.to_string(), decision, reason));
    }
    if decisions.is_empty() {
        return None;
    }
    let acceptance = value
        .get("acceptance")
        .and_then(Value::as_str)
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    Some(Ruling { decisions, acceptance })
}

/// Efeito de uma decisão já aplicada, para o registro e as mensagens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub finding: Finding,
    pub arbitration: Arbitration,
}

/// Resultado de [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub registry: Vec<Finding>,
    pub open: Vec<Finding>,
    /// Critérios novos, quando mudaram (ajuste e/ou verificação manual).
    pub acceptance: Option<String>,
    pub applied: Vec<Applied>,
    /// Os critérios finais não podiam ser gravados: `adjust` e `manual` viraram `keep`.
    pub downgraded: bool,
}

/// Aplica a decisão do Arquiteto ao registro, aos abertos e aos critérios, antes da revisão
/// `review`:
/// - `keep`: o apontamento continua aberto, com a decisão gravada (obrigatório dali em diante);
/// - `adjust`: os critérios viram `ruling.acceptance` (validado por `valid`); sem critério válido, a
///   decisão vira `keep` (nada é afrouxado sem o critério reescrito);
/// - `manual`: um critério `(manual)` documenta a verificação, e o apontamento sai da cobrança.
///
/// O apontamento ajustado ou manual fica `resolved` no registro (com a decisão) e sai dos abertos.
/// O texto final dos critérios (ajuste mais as linhas manuais) é validado antes: inválido, todas as
/// decisões viram `keep` (`downgraded`). A arbitragem não mexe no contador de revisões.
pub fn apply(
    registry: &[Finding],
    open: &[Finding],
    acceptance: &str,
    ruling: &Ruling,
    review: u32,
    valid: &dyn Fn(&str) -> bool,
) -> Outcome {
    let adjusted = ruling.acceptance.as_deref().filter(|a| valid(a));
    let decide = |decision: ArbitrationDecision| match decision {
        ArbitrationDecision::Adjust if adjusted.is_none() => ArbitrationDecision::Keep,
        other => other,
    };
    // O texto final dos critérios é montado e validado antes de qualquer dispensa: se não puder ser
    // gravado, nada é dispensado (ajuste e manual viram keep).
    let mut criteria = adjusted.unwrap_or(acceptance).trim_end().to_string();
    let mut changed = adjusted.is_some();
    for (id, decision, reason) in &ruling.decisions {
        let Some(entry) = registry.iter().find(|f| &f.id == id) else { continue };
        if decide(*decision) == ArbitrationDecision::Manual {
            let line = format!("- (manual) {}: {}", entry.id, short_title(&entry.description));
            let line = if reason.is_empty() { line } else { format!("{line} ({reason})") };
            if !criteria.is_empty() {
                criteria.push('\n');
            }
            criteria.push_str(&line);
            changed = true;
        }
    }
    let downgraded = changed && !valid(&criteria);
    let mut registry = registry.to_vec();
    let mut open = open.to_vec();
    let mut applied = Vec::new();
    for (id, decision, reason) in &ruling.decisions {
        let decision = if downgraded { ArbitrationDecision::Keep } else { decide(*decision) };
        let arbitration = Arbitration { decision, reason: reason.clone(), review };
        let Some(entry) = registry.iter_mut().find(|f| &f.id == id) else { continue };
        entry.arbitration = Some(arbitration.clone());
        // A arbitragem não conta como revisão: `last_review` fica como está.
        if decision.waives() {
            entry.state = Some(FindingState::Resolved);
            open.retain(|f| &f.id != id);
        } else if let Some(o) = open.iter_mut().find(|f| &f.id == id) {
            o.arbitration = Some(arbitration.clone());
        }
        applied.push(Applied { finding: entry.clone(), arbitration });
    }
    let acceptance = (changed && !downgraded).then_some(criteria);
    Outcome { registry, open, acceptance, applied, downgraded }
}

/// Apontamentos dispensados pelo Arquiteto (`adjust` ou `manual`): o Revisor não os cobra mais.
pub fn waived(registry: &[Finding]) -> Vec<Finding> {
    registry.iter().filter(|f| f.arbitration.as_ref().is_some_and(|a| a.decision.waives())).cloned().collect()
}

/// Todas as decisões do registro, para o Revisor e a mensagem: `F-03 [manual] título: motivo`.
pub fn decision_lines(registry: &[Finding]) -> Vec<String> {
    registry
        .iter()
        .filter_map(|f| {
            let a = f.arbitration.as_ref()?;
            let title = short_title(&f.description);
            Some(if a.reason.is_empty() {
                format!("{} [{}] {title}", f.id, a.decision.code())
            } else {
                format!("{} [{}] {title}: {}", f.id, a.decision.code(), a.reason)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str) -> Finding {
        Finding {
            id: id.into(),
            severity: "HIGH".into(),
            location: "src/a.rs:1".into(),
            description: format!("Falta cobrir {id}. Detalhe."),
            recommendation: "cobrir".into(),
            state: Some(FindingState::Open),
            recurrence: 2,
            ..Finding::default()
        }
    }

    #[test]
    fn parses_each_kind_and_rejects_incomplete_lines() {
        let fixed = parse_line("- F-01: fixed: src/a.rs:42").expect("fixed");
        assert_eq!((fixed.id.as_str(), fixed.kind, fixed.detail.as_str()), ("F-01", ResponseKind::Fixed, "src/a.rs:42"));
        let test = parse_line("2. `F-02`: test_added: tests/x.rs::caso").expect("test");
        assert_eq!((test.kind, test.detail.as_str()), (ResponseKind::TestAdded, "tests/x.rs::caso"));
        let c = parse_line("* **F-03**: contested (environment): o Docker não tem GPU").expect("contested");
        assert_eq!((c.kind, c.reason, c.detail.as_str()), (ResponseKind::Contested, Some(ContestReason::Environment), "o Docker não tem GPU"));
        assert_eq!(parse_line("- F-04 contested (out of scope): outra tarefa").and_then(|r| r.reason), Some(ContestReason::OutOfScope));
        assert!(parse_line("- F-05: contested: sem motivo tipado").is_none(), "contestação exige o tipo do motivo");
        assert!(parse_line("- F-06: fixed:").is_none(), "sem detalhe");
        assert!(parse_line("- F-07: talvez: x").is_none());
        assert!(parse_line("1. critério atendido: x.rs:1").is_none());
        assert_eq!(c.line(), "F-03: contested (environment): o Docker não tem GPU");
        assert_eq!(c.message_line(), "F-03 [contested, environment] o Docker não tem GPU");
        assert_eq!(parse_line(&c.line()), Some(c));
    }

    #[test]
    fn extracts_the_section_until_the_next_title() {
        let summary = "Feito.\n\n## Respostas aos apontamentos\n- F-01: fixed: src/a.rs:1\n- F-01: fixed: repetido\n- F-02: contested (incorrect): o código já valida\n\n**Critérios:**\n1. F-09: fixed: não conta";
        let r = extract(summary);
        assert_eq!(r.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["F-01", "F-02"]);
        assert_eq!(r[0].detail, "src/a.rs:1", "a primeira resposta de cada id vale");
        assert!(extract("sem seção\n- F-01: fixed: x").is_empty());
        assert_eq!(extract("Responses to findings:\n- F-01: fixed: a.rs").len(), 1);
    }

    /// Spec 129 T04: o título vale nas formas do dicionário (pt e en), pela linha inteira; uma frase
    /// da prosa que cite as palavras não abre a seção.
    #[test]
    fn title_in_both_languages_and_never_from_prose() {
        for title in ["## Respostas aos apontamentos", "**Responses to findings**", "### RESPONSES TO FINDINGS:", "respostas aos apontamentos"] {
            let r = extract(&format!("Done.\n\n{title}\n- F-01: fixed: src/a.rs:1"));
            assert_eq!(r.len(), 1, "{title}");
        }
        let prose = "I wrote the responses to findings below.\n- F-01: fixed: src/a.rs:1";
        assert!(extract(prose).is_empty());
        let both = format!("## {}\n- F-01: fixed: a.rs\n\n## {}\n- x", crate::i18n::text(crate::i18n::Lang::En, markers::DEV_RESPONSES), "Criteria");
        assert_eq!(extract(&both).len(), 1);
    }

    #[test]
    fn missing_contested_again_and_what_goes_to_the_architect() {
        let mut kept = finding("F-02");
        kept.arbitration = Some(Arbitration { decision: ArbitrationDecision::Keep, reason: "dá para testar".into(), review: 2 });
        let open = vec![finding("F-01"), kept, finding("F-03")];
        let responses = vec![
            parse_line("- F-02: contested (environment): de novo").expect("r"),
            parse_line("- F-03: contested (environment): sem GPU").expect("r"),
            parse_line("- F-09: fixed: fora da lista").expect("r"),
        ];
        let responses = for_open(responses, &open);
        assert_eq!(responses.len(), 2, "só os abertos");
        assert_eq!(missing(&open, &responses), vec!["F-01"]);
        assert_eq!(contested_again(&open, &responses), vec!["F-02"]);
        let go = to_arbitrate(&open, &responses);
        assert_eq!(go.iter().map(|(f, _)| f.id.as_str()).collect::<Vec<_>>(), vec!["F-03"], "contestação única");
        let merged = merge(&responses, &[parse_line("- F-03: fixed: src/gpu.rs:1").expect("r")]);
        assert_eq!(merged.iter().map(|r| (r.id.as_str(), r.kind)).collect::<Vec<_>>(), vec![("F-02", ResponseKind::Contested), ("F-03", ResponseKind::Fixed)]);
        assert_eq!(read_section(&render_section(&merged).expect("seção")), merged);
    }

    #[test]
    fn ruling_keeps_only_contested_ids() {
        let raw = "texto {\"decisions\":[{\"id\":\"F-03\",\"decision\":\"manual\",\"reason\":\"sem GPU\"},{\"id\":\"F-03\",\"decision\":\"keep\"},{\"id\":\"F-08\",\"decision\":\"keep\"},{\"id\":\"F-04\",\"decision\":\"talvez\"}],\"acceptance\":\" \"} fim";
        let r = parse_ruling(raw, &["F-03".into(), "F-04".into()]).expect("ruling");
        assert_eq!(r.decisions, vec![("F-03".to_string(), ArbitrationDecision::Manual, "sem GPU".to_string())]);
        assert!(r.acceptance.is_none());
        assert!(parse_ruling("nada", &["F-03".into()]).is_none());
        assert!(parse_ruling("{\"decisions\":[]}", &["F-03".into()]).is_none());
    }

    fn ok(_: &str) -> bool {
        true
    }

    #[test]
    fn apply_each_decision() {
        let registry = vec![finding("F-01"), finding("F-02"), finding("F-03")];
        let open = registry.clone();
        let ruling = Ruling {
            decisions: vec![
                ("F-01".into(), ArbitrationDecision::Keep, "dá para testar com mock".into()),
                ("F-02".into(), ArbitrationDecision::Adjust, "o ambiente prova a versão por software".into()),
                ("F-03".into(), ArbitrationDecision::Manual, "sem GPU no Docker".into()),
            ],
            acceptance: Some("1. Renderiza por software (verificado no Docker)".into()),
        };
        let out = apply(&registry, &open, "1. Renderiza com WebGPU nativo", &ruling, 4, &ok);
        assert_eq!(out.open.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["F-01"]);
        assert_eq!(out.open[0].arbitration.as_ref().map(|a| a.decision), Some(ArbitrationDecision::Keep));
        let states: Vec<_> = out.registry.iter().map(|f| (f.id.as_str(), f.state, f.arbitration.as_ref().map(|a| (a.decision, a.review)))).collect();
        assert_eq!(
            states,
            vec![
                ("F-01", Some(FindingState::Open), Some((ArbitrationDecision::Keep, 4))),
                ("F-02", Some(FindingState::Resolved), Some((ArbitrationDecision::Adjust, 4))),
                ("F-03", Some(FindingState::Resolved), Some((ArbitrationDecision::Manual, 4))),
            ]
        );
        assert_eq!(
            out.acceptance.as_deref(),
            Some("1. Renderiza por software (verificado no Docker)\n- (manual) F-03: Falta cobrir F-03 (sem GPU no Docker)")
        );
        assert_eq!(waived(&out.registry).iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["F-02", "F-03"]);
        assert_eq!(decision_lines(&out.registry)[2], "F-03 [manual] Falta cobrir F-03: sem GPU no Docker");
    }

    #[test]
    fn adjust_without_a_valid_criterion_becomes_keep() {
        let registry = vec![finding("F-02")];
        let ruling = Ruling { decisions: vec![("F-02".into(), ArbitrationDecision::Adjust, "x".into())], acceptance: Some("## quebra".into()) };
        let out = apply(&registry, &registry, "1. a", &ruling, 3, &|t: &str| !t.contains("## "));
        assert_eq!(out.applied[0].arbitration.decision, ArbitrationDecision::Keep);
        assert_eq!(out.open.len(), 1);
        assert!(out.acceptance.is_none());
    }

    /// Critério final que não pode ser gravado: nada é dispensado, tudo vira keep.
    #[test]
    fn invalid_final_criteria_dispense_nothing() {
        let registry = vec![finding("F-02"), finding("F-03")];
        let ruling = Ruling {
            decisions: vec![
                ("F-02".into(), ArbitrationDecision::Adjust, "x".into()),
                ("F-03".into(), ArbitrationDecision::Manual, "sem GPU".into()),
            ],
            acceptance: Some("1. curto".into()),
        };
        // Aceita o ajuste sozinho, mas recusa o texto somado com a linha manual (ex.: grande demais).
        let out = apply(&registry, &registry, "1. a", &ruling, 3, &|t: &str| !t.contains("(manual)"));
        assert!(out.downgraded && out.acceptance.is_none());
        assert_eq!(out.open.len(), 2, "nenhum dispensado");
        assert!(out.applied.iter().all(|a| a.arbitration.decision == ArbitrationDecision::Keep));
        assert!(out.registry.iter().all(|f| f.state == Some(FindingState::Open)));
        assert!(waived(&out.registry).is_empty());
    }

    #[test]
    fn arbitration_does_not_touch_the_review_counter() {
        let registry = vec![Finding { last_review: 1, ..finding("F-03") }];
        let ruling = Ruling { decisions: vec![("F-03".into(), ArbitrationDecision::Manual, "x".into())], acceptance: None };
        let out = apply(&registry, &registry, "1. a", &ruling, 2, &ok);
        assert_eq!(out.registry[0].last_review, 1);
        assert_eq!(out.registry[0].arbitration.as_ref().map(|a| a.review), Some(2));
    }

    /// Spec 126 T02: o gatilho antecipado pega o recorrente e o "corrigido" duas vezes, uma vez só,
    /// e nunca o que já tem decisão do Arquiteto.
    #[test]
    fn due_early_once_per_finding() {
        let mut fresh = finding("F-01");
        fresh.recurrence = 2;
        let mut recurring = finding("F-02");
        recurring.recurrence = 3;
        let mut claimed = finding("F-03");
        claimed.fixed_claims = 2;
        let mut decided = finding("F-04");
        decided.recurrence = 5;
        decided.arbitration = Some(Arbitration { decision: ArbitrationDecision::Keep, reason: "x".into(), review: 2 });
        let mut resolved = finding("F-05");
        resolved.recurrence = 4;
        resolved.state = Some(FindingState::Resolved);
        let mut registry = vec![fresh, recurring, claimed, decided, resolved];
        let due = due_early(&registry);
        assert_eq!(due.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), vec!["F-02", "F-03"]);
        assert_eq!(high_count(&due), 2);
        let ids: Vec<String> = due.iter().map(|f| f.id.clone()).collect();
        let mut open = registry.clone();
        mark_early(&mut registry, &mut open, &ids, 4);
        assert!(due_early(&registry).is_empty(), "uma vez por apontamento");
        assert_eq!(open.iter().filter(|f| f.early_review == 4).count(), 2);
        let text = recurring_facts("T05", "Render", "obj", "1. a", &[(registry[2].clone(), parse_line("- F-03: test_added: tests/a.rs::x"))]);
        assert!(text.contains("respondido como corrigido ou com teste em 2 delas") && text.contains("Última resposta do Desenvolvedor: test_added: tests/a.rs::x"), "{text}");
    }

    #[test]
    fn facts_carry_the_contest() {
        let f = finding("F-03");
        let r = parse_line("- F-03: contested (environment): sem GPU").expect("r");
        let text = facts("T05", "Render", "obj", "1. a", &[(f, r)]);
        assert!(text.contains("- F-03 [HIGH] src/a.rs:1: Falta cobrir F-03. Detalhe.") && text.contains("Contestação (environment): sem GPU"), "{text}");
    }
}
