//! Recuperação com SPLIT, ADJUST e ASK além de RETRY (spec 124 T03).
//!
//! O Arquiteto, diante de uma tarefa bloqueada, pode:
//! - `SPLIT`: a tarefa atual fica com um objetivo e critérios menores ([`TaskSplit::current`]) e
//!   uma ou mais tarefas novas recebem o restante ([`SplitTask`]), no formato das tarefas do plano.
//!   As novas dependem da atual (e cada uma da anterior), e quem dependia da atual passa a depender
//!   também das novas. A branch da tarefa atual é mantida: o trabalho feito nela é aproveitado;
//! - `ADJUST`: os critérios são reescritos para o que o ambiente prova e o restante vira verificação
//!   manual documentada. É o mesmo mecanismo da arbitragem da T02 (`finding_reply::apply`): a mesma
//!   validação dos critérios, a mesma decisão no registro e o mesmo registro na spec;
//! - `ASK`: a execução pausa com uma pergunta objetiva ao humano na Conversa.
//!
//! Este módulo tem os tipos validados e as funções puras; quem grava é o runner
//! (`TaskRunner::record_split` e `TaskRunner::record_adjust`). Também monta os SINAIS que o
//! Arquiteto recebe para escolher ([`signals`]). Os textos vêm do modelo: são DADO.

use crate::agents::finding_track;
use crate::agents::runner::{
    validate_scope, validate_text_block, TaskRevision, ACCEPTANCE, FINDINGS, OBJECTIVE, RECOVERY_PREFIX,
};
use crate::sdd::{ArbitrationDecision, Finding, Task, TaskMeta, TaskStatus, Verdict};

/// Máximo de tarefas novas num SPLIT.
pub const MAX_SPLIT_TASKS: usize = 3;
/// Limite do título de uma tarefa nova.
pub const MAX_TITLE_BYTES: usize = 200;
/// Limite da pergunta do ASK.
pub const MAX_QUESTION_BYTES: usize = 1024;
/// Limite do motivo de cada apontamento num ADJUST.
const MAX_REASON_BYTES: usize = 600;
/// Revisões seguidas a partir das quais um apontamento é recorrente.
pub const RECURRENT_REVIEWS: u32 = 3;
/// Critérios a partir dos quais a tarefa é grande.
pub const MANY_CRITERIA: usize = 6;

/// Linhas fixas da análise de recuperação (marcadores estruturais lidos pela web, que os traduz em
/// `web/src/recoveryAnalysis.ts`, como as das outras decisões).
pub const SPLIT_DECISION_LINE: &str = "- Decisão: tarefa dividida (SPLIT)";
pub const ADJUST_DECISION_LINE: &str = "- Decisão: critérios ajustados ao que o ambiente prova (ADJUST)";
pub const ASK_DECISION_LINE: &str = "- Decisão: pergunta ao humano (ASK)";
/// Rótulo da pergunta do ASK (o valor é texto do modelo).
pub const QUESTION_LABEL: &str = "- Pergunta ao humano: ";
/// Rótulo das tarefas criadas pelo SPLIT: `T07 (título), T08 (título)`.
pub const NEW_TASKS_LABEL: &str = "- Tarefas novas: ";
/// Rótulo dos apontamentos dispensados pelo ADJUST: `F-02 [adjust], F-03 [manual]`.
pub const WAIVED_LABEL: &str = "- Apontamentos dispensados: ";
/// ADJUST cujos critérios não puderam ser gravados: a tarefa segue com a estratégia (como `keep`).
pub const ADJUST_REFUSED_LINE: &str =
    "- Ajuste não aplicado: os critérios não puderam ser gravados; a tarefa segue com a estratégia (RETRY)";
/// Linha de opções do contexto de um ASK sem opções.
pub const ASK_OPTIONS_LINE: &str =
    "  - opções: responda à pergunta acima no campo de orientação; a tarefa volta a executar com a sua resposta";

/// Tarefa nova de um SPLIT, no formato das tarefas do plano.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitTask {
    pub title: String,
    pub objective: String,
    pub acceptance: String,
    pub allowed_scope: Vec<String>,
}

/// Divisão da tarefa: a definição menor da atual e as tarefas novas com o restante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSplit {
    pub current: TaskRevision,
    pub new_tasks: Vec<SplitTask>,
}

impl TaskSplit {
    /// A atual pelas regras do REVISE_TASK; de 1 a [`MAX_SPLIT_TASKS`] novas, cada uma com título de
    /// uma linha (até [`MAX_TITLE_BYTES`]), objetivo e critérios válidos e `allowed_scope` válido.
    pub fn validate(&self) -> Result<(), String> {
        self.current.validate()?;
        if self.new_tasks.is_empty() || self.new_tasks.len() > MAX_SPLIT_TASKS {
            return Err(format!("new_tasks deve ter de 1 a {MAX_SPLIT_TASKS} tarefas"));
        }
        for t in &self.new_tasks {
            let title = t.title.trim();
            if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.contains('\n') || title.contains("<!--") {
                return Err("título de tarefa nova inválido".into());
            }
            validate_text_block("objetivo", &t.objective)?;
            validate_text_block("critérios", &t.acceptance)?;
            validate_scope(&t.allowed_scope)?;
        }
        Ok(())
    }
}

/// Ajuste dos critérios ao que o ambiente prova.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CriteriaAdjustment {
    /// Critérios reescritos (texto completo da seção).
    pub acceptance: String,
    /// Apontamentos dispensados: `(id, adjust | manual, motivo)`.
    pub findings: Vec<(String, ArbitrationDecision, String)>,
    /// Motivo do ajuste (a causa raiz da análise), para o registro na spec.
    pub reason: String,
}

impl CriteriaAdjustment {
    /// Critérios válidos (a mesma regra da arbitragem) e só `adjust`/`manual`, sem id repetido.
    pub fn validate(&self) -> Result<(), String> {
        validate_text_block("critérios", &self.acceptance)?;
        for (i, (id, decision, reason)) in self.findings.iter().enumerate() {
            if !decision.waives() {
                return Err(format!("apontamento {id}: só adjust ou manual"));
            }
            if id.trim().is_empty() || id.len() > 16 || self.findings[..i].iter().any(|(other, _, _)| other == id) {
                return Err(format!("apontamento {id}: id inválido ou repetido"));
            }
            if reason.len() > MAX_REASON_BYTES {
                return Err(format!("apontamento {id}: motivo grande demais"));
            }
        }
        Ok(())
    }
}

/// Ids livres para `count` tarefas novas: a seguir ao maior `Tnn` da spec, pulando os ocupados.
pub fn next_ids(tasks: &[Task], count: usize, taken: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut number = tasks
        .iter()
        .filter_map(|t| t.frontmatter.id.strip_prefix('T')?.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        number += 1;
        let id = format!("T{number:02}");
        if !taken(&id) && !tasks.iter().any(|t| t.frontmatter.id == id) {
            out.push(id);
        }
    }
    out
}

/// Tarefa nova no formato do plano (`TODO`, sem branch nem tentativas). `origin` é a frase, no idioma
/// do projeto, que liga a tarefa à divisão; fica no fim do objetivo.
pub fn new_task(spec_id: &str, id: &str, depends_on: Vec<String>, covers: Vec<String>, part: &SplitTask, origin: &str) -> Task {
    let scope: Vec<String> = part.allowed_scope.iter().map(|g| g.trim().to_string()).collect();
    Task {
        frontmatter: TaskMeta {
            id: id.to_string(),
            spec_id: spec_id.to_string(),
            title: part.title.trim().to_string(),
            status: TaskStatus::Todo,
            depends_on,
            branch: String::new(),
            attempts: 0,
            max_attempts: 3,
            last_verdict: Verdict::None,
            allowed_scope: scope,
            covers,
            reviewed_sha: None,
            rejected_sha: None,
            tested_sha: None,
            tested_signature: None,
            tested_at: None,
            test_evidence: None,
            test_run: None,
            next_action: None,
            next_action_code: None,
            diff_lines: Vec::new(),
            kind: None,
        },
        body: format!(
            "## {OBJECTIVE}\n{}\n\n{}\n\n## {ACCEPTANCE}\n{}\n",
            part.objective.trim(),
            origin.trim(),
            part.acceptance.trim()
        ),
    }
}

/// Linha das tarefas criadas: `- Tarefas novas: T07 (título), T08 (título)`.
pub fn new_tasks_line(created: &[(String, String)]) -> String {
    let items: Vec<String> = created.iter().map(|(id, title)| format!("{id} ({})", title.trim())).collect();
    format!("{NEW_TASKS_LABEL}{}", items.join(", "))
}

/// Linha dos apontamentos dispensados: `- Apontamentos dispensados: F-02 [adjust], F-03 [manual]`.
pub fn waived_line(waived: &[(String, ArbitrationDecision)]) -> Option<String> {
    if waived.is_empty() {
        return None;
    }
    let items: Vec<String> = waived.iter().map(|(id, d)| format!("{id} [{}]", d.code())).collect();
    Some(format!("{WAIVED_LABEL}{}", items.join(", ")))
}

/// Uso de um papel na tarefa até aqui.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleCost {
    pub agent: String,
    pub calls: u64,
    pub input: u64,
    pub output: u64,
}

/// Itens de lista de PRIMEIRO NÍVEL dos critérios (linhas sem recuo que começam com `-`, `*` ou
/// `1.`). Os subitens recuados detalham um critério, não são critérios novos (spec 124 T06). Única
/// contagem do tamanho da tarefa: os sinais da recuperação e o apontamento `too_large` do plano.
pub fn count_criteria(acceptance: &str) -> usize {
    acceptance
        .lines()
        .filter(|l| !l.starts_with([' ', '\t']))
        .map(str::trim_end)
        .filter(|l| {
            l.starts_with("- ")
                || l.starts_with("* ")
                || l.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .count()
}

/// Decisões das análises de recuperação anteriores: `#1 RETRY`, `#2 REVISE_TASK`...
fn previous_decisions(body: &str) -> Vec<String> {
    let header = format!("## {RECOVERY_PREFIX}");
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in body.lines() {
        if line.starts_with("## ") {
            current = line.strip_prefix(&header).and_then(|n| n.split_whitespace().next()).map(str::to_string);
            continue;
        }
        let Some(n) = &current else { continue };
        let Some(decision) = line.trim_end().strip_prefix("- Decisão: ") else { continue };
        let code = match decision.rsplit_once('(') {
            Some((_, rest)) if rest.ends_with(')') && rest[..rest.len() - 1].chars().all(|c| c.is_ascii_uppercase() || c == '_') => {
                rest[..rest.len() - 1].to_string()
            }
            _ => "ESCALATE".to_string(),
        };
        out.push(format!("#{n} {code}"));
        current = None;
    }
    out
}

fn yaml_findings(body: &str, title: &str) -> Vec<Finding> {
    crate::agents::runner::section_text(body, title)
        .filter(|t| !t.trim().is_empty())
        .and_then(|t| serde_yaml::from_str::<Vec<Finding>>(&t).ok())
        .unwrap_or_default()
}

fn plural(n: u64, one: &str, other: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {other}")
    }
}

/// SINAIS para a escolha da recuperação (texto de prompt, não de pessoa): recorrência dos
/// apontamentos (pelo id e pela semelhança com um resolvido), contestações e decisões do Arquiteto,
/// tamanho da tarefa, tentativas e recuperações anteriores e o custo até aqui.
pub fn signals(task: &Task, cost: &[RoleCost]) -> String {
    let meta = &task.frontmatter;
    let open = yaml_findings(&task.body, FINDINGS);
    let registry = finding_track::seed(&yaml_findings(&task.body, finding_track::REGISTRY), &open);
    let reviews = registry.iter().map(|f| f.last_review).max().unwrap_or(0);
    let mut lines = vec![format!(
        "- Revisões da tarefa: {reviews}; tentativas nesta rodada: {} de {}",
        meta.attempts, meta.max_attempts
    )];
    let previous = previous_decisions(&task.body);
    if previous.is_empty() {
        lines.push("- Análises de recuperação anteriores: nenhuma".into());
    } else {
        lines.push(format!("- Análises de recuperação anteriores: {} ({})", previous.len(), previous.join(", ")));
    }
    let pending = finding_track::pending(&registry);
    let mut recurrent: Vec<&Finding> = pending.iter().filter(|f| f.recurrence >= RECURRENT_REVIEWS).collect();
    recurrent.sort_by(|a, b| b.recurrence.cmp(&a.recurrence));
    let mut line = format!("- Apontamentos abertos: {}", pending.len());
    if !recurrent.is_empty() {
        let items: Vec<String> = recurrent
            .iter()
            .map(|f| format!("{} ({} revisões: {})", f.id, f.recurrence, finding_track::short_title(&f.description)))
            .collect();
        line.push_str(&format!("; abertos há {RECURRENT_REVIEWS} ou mais revisões: {}", items.join("; ")));
    }
    lines.push(line);
    // Spec 126 T02: "corrigido" sem fechar, em duas revisões ou mais.
    let claimed: Vec<String> = pending
        .iter()
        .filter(|f| f.fixed_claims >= crate::agents::finding_reply::EARLY_FIXED_CLAIMS)
        .map(|f| format!("{} ({} vezes: {})", f.id, f.fixed_claims, finding_track::short_title(&f.description)))
        .collect();
    if !claimed.is_empty() {
        lines.push(format!("- Respondidos como corrigidos ou com teste e ainda abertos: {}", claimed.join("; ")));
    }
    let returned = finding_track::returned_like_resolved(&registry);
    if !returned.is_empty() {
        let items: Vec<String> = returned
            .iter()
            .map(|(now, before)| {
                format!(
                    "{} parece com {} (resolvido depois de {}): {}",
                    now.id,
                    before.id,
                    plural(u64::from(before.recurrence.max(1)), "revisão", "revisões"),
                    finding_track::short_title(&now.description)
                )
            })
            .collect();
        lines.push(format!("- Voltaram com id novo, parecidos com resolvidos: {}", items.join("; ")));
    }
    let contested: Vec<String> = crate::agents::runner::section_text(&task.body, crate::agents::finding_reply::RESPONSES)
        .map(|t| crate::agents::finding_reply::read_section(&t))
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.kind == crate::agents::finding_reply::ResponseKind::Contested)
        .map(|r| r.message_line())
        .collect();
    if !contested.is_empty() {
        lines.push(format!("- Contestações do Desenvolvedor: {}", contested.join("; ")));
    }
    let decisions = crate::agents::finding_reply::decision_lines(&registry);
    if !decisions.is_empty() {
        lines.push(format!("- Decisões do Arquiteto sobre contestações: {}", decisions.join("; ")));
    }
    let acceptance = crate::agents::runner::section_text(&task.body, ACCEPTANCE).unwrap_or_default();
    let criteria = count_criteria(&acceptance);
    lines.push(format!("- Critérios de aceitação: {criteria}; itens do allowed_scope: {}", meta.allowed_scope.len()));
    if cost.is_empty() {
        lines.push("- Custo da tarefa até aqui: sem registro".into());
    } else {
        let items: Vec<String> = cost
            .iter()
            .map(|c| format!("{}: {}, {} tokens de entrada, {} de saída", c.agent, plural(c.calls, "chamada", "chamadas"), c.input, c.output))
            .collect();
        lines.push(format!("- Custo da tarefa até aqui: {}", items.join("; ")));
    }
    let big = criteria >= MANY_CRITERIA;
    if !recurrent.is_empty() || !returned.is_empty() || big {
        lines.push(format!(
            "- Atenção: {}. Considere SPLIT ou ADJUST antes de outro RETRY.",
            match (!recurrent.is_empty() || !returned.is_empty(), big) {
                (true, true) => "apontamentos recorrentes e tarefa com muitos critérios",
                (true, false) => "apontamentos recorrentes por 3 ou mais revisões",
                _ => "tarefa com muitos critérios",
            }
        ));
    }
    format!("\n## Sinais para a decisão\n{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_with(body: &str) -> Task {
        let mut t = new_task(
            "s1",
            "T05",
            vec!["T04".into()],
            vec![],
            &SplitTask { title: "x".into(), objective: "o".into(), acceptance: "- a".into(), allowed_scope: vec!["src/**".into()] },
            "origem",
        );
        t.frontmatter.status = TaskStatus::Blocked;
        t.frontmatter.attempts = 3;
        t.body = body.to_string();
        t
    }

    fn part(title: &str) -> SplitTask {
        SplitTask { title: title.into(), objective: "Obj".into(), acceptance: "- [ ] critério".into(), allowed_scope: vec!["tests/**".into()] }
    }

    #[test]
    fn split_validation() {
        let current = TaskRevision { objective: "Menor".into(), acceptance: "- a".into(), allowed_scope: None };
        let ok = TaskSplit { current: current.clone(), new_tasks: vec![part("Provas de ponta a ponta")] };
        assert!(ok.validate().is_ok());
        let none = TaskSplit { current: current.clone(), new_tasks: vec![] };
        assert!(none.validate().is_err());
        let many = TaskSplit { current: current.clone(), new_tasks: vec![part("a"); MAX_SPLIT_TASKS + 1] };
        assert!(many.validate().is_err());
        let mut bad = part("a\nb");
        assert!(TaskSplit { current: current.clone(), new_tasks: vec![bad.clone()] }.validate().is_err());
        bad.title = "ok".into();
        bad.allowed_scope = vec![".sdd/**".into()];
        assert!(TaskSplit { current: current.clone(), new_tasks: vec![bad.clone()] }.validate().is_err());
        bad.allowed_scope = vec!["src/**".into()];
        bad.acceptance = "## forjada".into();
        assert!(TaskSplit { current, new_tasks: vec![bad] }.validate().is_err());
    }

    #[test]
    fn adjustment_validation() {
        let ok = CriteriaAdjustment { acceptance: "- a".into(), findings: vec![("F-01".into(), ArbitrationDecision::Manual, "sem GPU".into())], reason: "r".into() };
        assert!(ok.validate().is_ok());
        let keep = CriteriaAdjustment { findings: vec![("F-01".into(), ArbitrationDecision::Keep, String::new())], ..ok.clone() };
        assert!(keep.validate().is_err());
        let twice = CriteriaAdjustment {
            findings: vec![("F-01".into(), ArbitrationDecision::Manual, String::new()), ("F-01".into(), ArbitrationDecision::Adjust, String::new())],
            ..ok.clone()
        };
        assert!(twice.validate().is_err());
        assert!(CriteriaAdjustment { acceptance: " ".into(), ..ok }.validate().is_err());
    }

    #[test]
    fn ids_follow_the_highest_and_skip_taken() {
        let tasks = vec![task_with(""), {
            let mut t = task_with("");
            t.frontmatter.id = "T06".into();
            t
        }];
        assert_eq!(next_ids(&tasks, 2, &|id| id == "T07"), vec!["T08", "T09"]);
    }

    #[test]
    fn new_task_has_the_plan_format() {
        let t = new_task("s1", "T07", vec!["T05".into()], vec!["G5".into()], &part("Provas"), "Parte da T05.");
        let rendered = t.render().expect("render");
        let back = Task::parse(&rendered).expect("parse");
        assert_eq!(back.frontmatter.status, TaskStatus::Todo);
        assert_eq!(back.frontmatter.depends_on, vec!["T05"]);
        assert_eq!(back.frontmatter.covers, vec!["G5"]);
        assert_eq!(crate::agents::runner::section_text(&back.body, ACCEPTANCE).as_deref().map(str::trim), Some("- [ ] critério"));
        assert!(crate::agents::runner::section_text(&back.body, OBJECTIVE).is_some_and(|o| o.contains("Obj") && o.contains("Parte da T05.")));
    }

    #[test]
    fn signals_carry_recurrence_contests_size_history_and_cost() {
        let registry = "- id: F-02\n  severity: HIGH\n  location: tests/e2e/two.spec.ts:40\n  description: Convergência entre dois clientes não é testada.\n  recommendation: testar\n  state: resolved\n  recurrence: 4\n  last_review: 5\n\
- id: F-04\n  severity: HIGH\n  location: src/server/edit.ts:9\n  description: Validação de edição sem cobertura.\n  recommendation: cobrir\n  state: open\n  recurrence: 6\n  last_review: 7\n\
- id: F-07\n  severity: HIGH\n  location: tests/e2e/two.spec.ts:90\n  description: Falta testar a convergência entre dois clientes.\n  recommendation: testar\n  state: new\n  recurrence: 1\n  last_review: 7\n";
        let body = format!(
            "## {OBJECTIVE}\nx\n## {ACCEPTANCE}\n- a\n- b\n- c\n- d\n- e\n- f\n## {}\n{registry}\n## Respostas aos Apontamentos\n- F-04: contested (environment): sem GPU no Docker\n## {RECOVERY_PREFIX}1\n- Causa raiz: x\n- Decisão: nova estratégia (RETRY)\n## {RECOVERY_PREFIX}2\n- Causa raiz: y\n- Decisão: escalar ao humano\n",
            finding_track::REGISTRY
        );
        let cost = vec![RoleCost { agent: "developer".into(), calls: 637, input: 16_200_000, output: 90_000 }];
        let s = signals(&task_with(&body), &cost);
        assert!(s.contains("- Revisões da tarefa: 7; tentativas nesta rodada: 3 de 3"), "{s}");
        assert!(s.contains("Análises de recuperação anteriores: 2 (#1 RETRY, #2 ESCALATE)"), "{s}");
        assert!(s.contains("abertos há 3 ou mais revisões: F-04 (6 revisões"), "{s}");
        assert!(s.contains("F-07 parece com F-02 (resolvido depois de 4 revisões)"), "{s}");
        assert!(s.contains("Contestações do Desenvolvedor: F-04 [contested, environment] sem GPU no Docker"), "{s}");
        assert!(s.contains("Critérios de aceitação: 6"), "{s}");
        assert!(s.contains("developer: 637 chamadas, 16200000 tokens de entrada, 90000 de saída"), "{s}");
        assert!(s.contains("Considere SPLIT ou ADJUST antes de outro RETRY"), "{s}");
        let quiet = signals(&task_with(&format!("## {ACCEPTANCE}\n- a\n")), &[]);
        assert!(!quiet.contains("Atenção") && quiet.contains("sem registro") && quiet.contains("nenhuma"), "{quiet}");
    }

    #[test]
    fn lines_for_the_record() {
        assert_eq!(new_tasks_line(&[("T07".into(), "Provas".into()), ("T08".into(), "Capturas".into())]), "- Tarefas novas: T07 (Provas), T08 (Capturas)");
        assert_eq!(waived_line(&[("F-03".into(), ArbitrationDecision::Manual)]).as_deref(), Some("- Apontamentos dispensados: F-03 [manual]"));
        assert!(waived_line(&[]).is_none());
        assert_eq!(count_criteria("- a\n1. b\n* c\ntexto\n10. d"), 4);
        // Subitens recuados (espaço ou tabulação) detalham o critério de cima: não contam.
        assert_eq!(count_criteria("1. a\n   - detalhe\n   - outro\n2. b\n\t* sub\n  3. recuado\n- c"), 3);
    }
}
