//! "O que mudou no plano" entre versões (spec 086 T09). Funções puras e determinísticas, sem
//! modelo: o servidor monta um [`PlanSnapshot`] antes do turno do chat e outro depois, e
//! [`diff_lines`] descreve a diferença em linhas curtas ("T04 nova: <título>", "T02 removida",
//! "T03: critérios alterados (2 novos, 1 removido)", "test_command alterado"…).
//!
//! Só conteúdo conta: espaços, marcadores de lista, caixas `[ ]` e linhas em branco são
//! normalizados, e listas de `depends_on`/`allowed_scope`/`covers` são comparadas sem ordem.

use std::collections::BTreeMap;

use super::coverage::global_criteria;
use super::{Spec, Task};
use crate::agents::runner::{section_text, ACCEPTANCE};

/// Máximo de linhas do resumo (incluindo a linha "…" quando há mais).
pub const MAX_CHANGE_LINES: usize = 20;
/// Marcador da última linha quando o resumo é cortado.
pub const MORE_LINE: &str = "…";
/// Tamanho máximo (em caracteres) do título citado em "Txx nova: <título>".
pub const TITLE_CHARS: usize = 80;

/// Retrato de uma tarefa: só o que importa para o plano.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub id: String,
    pub title: String,
    pub depends_on: Vec<String>,
    pub allowed_scope: Vec<String>,
    /// Linhas normalizadas de "## Critérios de Aceitação" da tarefa.
    pub criteria: Vec<String>,
    pub covers: Vec<String>,
}

/// Retrato estruturado do plano de uma spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanSnapshot {
    pub title: String,
    pub test_command: Option<String>,
    pub global_criteria: Vec<String>,
    pub tasks: Vec<TaskSnapshot>,
}

impl PlanSnapshot {
    /// Monta o retrato a partir da spec e das tarefas já lidas (e, se for o caso, redigidas).
    pub fn from_parts(spec: &Spec, tasks: &[Task]) -> Self {
        Self {
            title: flat(&spec.frontmatter.title),
            test_command: spec.frontmatter.test_command.as_deref().map(flat).filter(|c| !c.is_empty()),
            global_criteria: global_criteria(&spec.body).iter().map(|c| flat(c)).collect(),
            tasks: tasks.iter().map(TaskSnapshot::from_task).collect(),
        }
    }
}

impl TaskSnapshot {
    pub fn from_task(task: &Task) -> Self {
        let meta = &task.frontmatter;
        Self {
            id: flat(&meta.id),
            title: flat(&meta.title),
            depends_on: sorted(&meta.depends_on),
            allowed_scope: sorted(&meta.allowed_scope),
            criteria: section_text(&task.body, ACCEPTANCE).map(|t| criterion_lines(&t)).unwrap_or_default(),
            covers: sorted(&meta.covers),
        }
    }
}

/// Espaços colapsados numa linha.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Itens aparados, sem vazios, em ordem (comparação sem ordem nem formatação).
fn sorted(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|s| flat(s)).filter(|s| !s.is_empty()).collect();
    out.sort();
    out.dedup();
    out
}

/// Linhas de conteúdo de uma seção de critérios: sem marcador (`-`, `*`, `1.`, `1)`), sem caixa
/// `[ ]`/`[x]`, espaços colapsados; linhas vazias descartadas. Sublistas contam como linhas.
pub fn criterion_lines(text: &str) -> Vec<String> {
    text.lines().map(strip_marker).filter(|l| !l.is_empty()).collect()
}

fn strip_marker(line: &str) -> String {
    let t = line.trim();
    let rest = if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
        r
    } else {
        let digits = t.bytes().take_while(u8::is_ascii_digit).count();
        match t[digits..].strip_prefix(". ").or_else(|| t[digits..].strip_prefix(") ")) {
            Some(r) if digits > 0 => r,
            _ => t,
        }
    };
    let rest = rest.trim_start();
    let rest = ["[ ]", "[x]", "[X]"].iter().find_map(|b| rest.strip_prefix(b)).unwrap_or(rest);
    flat(rest)
}

/// Quantos itens de `new` não estão em `old` e vice-versa (como multiconjuntos).
fn added_removed(old: &[String], new: &[String]) -> (usize, usize) {
    let mut count: BTreeMap<&str, i64> = BTreeMap::new();
    for item in old {
        *count.entry(item.as_str()).or_default() -= 1;
    }
    for item in new {
        *count.entry(item.as_str()).or_default() += 1;
    }
    let added = count.values().filter(|n| **n > 0).map(|n| *n as usize).sum();
    let removed = count.values().filter(|n| **n < 0).map(|n| n.unsigned_abs() as usize).sum();
    (added, removed)
}

/// "2 novos, 1 removido" / "1 novo" / "reordenados" (mesmos itens, outra ordem).
fn list_delta(old: &[String], new: &[String]) -> Option<String> {
    if old == new {
        return None;
    }
    let (added, removed) = added_removed(old, new);
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("{added} {}", if added == 1 { "novo" } else { "novos" }));
    }
    if removed > 0 {
        parts.push(format!("{removed} {}", if removed == 1 { "removido" } else { "removidos" }));
    }
    if parts.is_empty() {
        return Some("reordenados".to_string());
    }
    Some(parts.join(", "))
}

/// Título curto (já redigido pelo chamador; o corte vem depois).
fn short_title(title: &str) -> String {
    if title.chars().count() <= TITLE_CHARS {
        return title.to_string();
    }
    let cut: String = title.chars().take(TITLE_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// Linhas do que mudou de `old` para `new`, na ordem: spec (título, `test_command`, critérios
/// globais), depois tarefas por id. Sem mudança de conteúdo, lista vazia. No máximo
/// [`MAX_CHANGE_LINES`] linhas; se houver mais, a última é [`MORE_LINE`].
pub fn diff_lines(old: &PlanSnapshot, new: &PlanSnapshot) -> Vec<String> {
    let mut lines = Vec::new();
    if old.title != new.title {
        lines.push("Título da spec alterado".to_string());
    }
    if old.test_command != new.test_command {
        lines.push("test_command alterado".to_string());
    }
    if let Some(delta) = list_delta(&old.global_criteria, &new.global_criteria) {
        lines.push(format!("Critérios globais: {delta}"));
    }
    let old_tasks: BTreeMap<&str, &TaskSnapshot> = old.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let new_tasks: BTreeMap<&str, &TaskSnapshot> = new.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut ids: Vec<&str> = old_tasks.keys().chain(new_tasks.keys()).copied().collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        match (old_tasks.get(id), new_tasks.get(id)) {
            (Some(_), None) => lines.push(format!("{id} removida")),
            (None, Some(t)) => lines.push(format!("{id} nova: {}", short_title(&t.title))),
            (Some(a), Some(b)) => task_lines(id, a, b, &mut lines),
            (None, None) => {}
        }
    }
    if lines.len() > MAX_CHANGE_LINES {
        lines.truncate(MAX_CHANGE_LINES - 1);
        lines.push(MORE_LINE.to_string());
    }
    lines
}

fn task_lines(id: &str, a: &TaskSnapshot, b: &TaskSnapshot, lines: &mut Vec<String>) {
    if a.title != b.title {
        lines.push(format!("{id}: título alterado"));
    }
    if a.depends_on != b.depends_on {
        lines.push(format!("{id}: dependências alteradas"));
    }
    if a.allowed_scope != b.allowed_scope {
        lines.push(format!("{id}: escopo alterado"));
    }
    if let Some(delta) = list_delta(&a.criteria, &b.criteria) {
        lines.push(format!("{id}: critérios alterados ({delta})"));
    }
    if a.covers != b.covers {
        lines.push(format!("{id}: cobertura alterada"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, title: &str, criteria: &[&str]) -> TaskSnapshot {
        TaskSnapshot {
            id: id.into(),
            title: title.into(),
            depends_on: Vec::new(),
            allowed_scope: vec!["src/**".into()],
            criteria: criteria.iter().map(|c| c.to_string()).collect(),
            covers: Vec::new(),
        }
    }

    fn plan(tasks: Vec<TaskSnapshot>) -> PlanSnapshot {
        PlanSnapshot {
            title: "Demo".into(),
            test_command: Some("cargo test".into()),
            global_criteria: vec!["testes verdes".into()],
            tasks,
        }
    }

    #[test]
    fn same_plan_has_no_lines() {
        let p = plan(vec![task("T01", "A", &["um"])]);
        assert!(diff_lines(&p, &p.clone()).is_empty());
    }

    #[test]
    fn criterion_lines_ignore_formatting() {
        let a = criterion_lines("1. **Um:**\n   - item  x\n\n2) [ ] dois\n");
        let b = criterion_lines("- **Um:**\n  * item x\n- [x] dois\n");
        assert_eq!(a, vec!["**Um:**", "item x", "dois"]);
        assert_eq!(a, b);
    }

    #[test]
    fn new_and_removed_tasks() {
        let old = plan(vec![task("T01", "A", &[]), task("T02", "B", &[])]);
        let new = plan(vec![task("T01", "A", &[]), task("T04", "Nova tarefa", &[])]);
        assert_eq!(diff_lines(&old, &new), vec!["T02 removida", "T04 nova: Nova tarefa"]);
    }

    #[test]
    fn task_field_lines() {
        let old = plan(vec![task("T03", "A", &["um", "dois"])]);
        let mut t = task("T03", "B", &["dois", "três", "quatro"]);
        t.allowed_scope = vec!["web/**".into()];
        t.depends_on = vec!["T01".into()];
        t.covers = vec!["G1".into()];
        let new = plan(vec![t]);
        assert_eq!(
            diff_lines(&old, &new),
            vec![
                "T03: título alterado",
                "T03: dependências alteradas",
                "T03: escopo alterado",
                "T03: critérios alterados (2 novos, 1 removido)",
                "T03: cobertura alterada",
            ]
        );
    }

    #[test]
    fn reordered_criteria_line() {
        let a = task("T01", "A", &["um", "dois"]);
        let b = task("T01", "A", &["dois", "um"]);
        assert_eq!(diff_lines(&plan(vec![a]), &plan(vec![b])), vec!["T01: critérios alterados (reordenados)"]);
    }

    #[test]
    fn from_task_ignores_scope_order_and_criteria_formatting() {
        let base = crate::agents::prompts::EXAMPLE_TASK_MD.replace("  - \"src/**\"", "  - \"src/**\"\n  - \"web/**\"");
        let other = base
            .replace("  - \"src/**\"\n  - \"web/**\"", "  - \"web/**\"\n  - \"src/**\"")
            .replace("- [ ] Critério verificável.", "1.  [x]  Critério   verificável.\n");
        let a = TaskSnapshot::from_task(&Task::parse(&base).expect("a"));
        let b = TaskSnapshot::from_task(&Task::parse(&other).expect("b"));
        assert_eq!(a.allowed_scope, vec!["src/**", "web/**"]);
        assert_eq!(a.criteria, vec!["Critério verificável."]);
        assert_eq!(a, b);
    }

    #[test]
    fn spec_level_lines() {
        let old = plan(Vec::new());
        let mut new = plan(Vec::new());
        new.title = "Outra".into();
        new.test_command = Some("npm test".into());
        new.global_criteria.push("build verde".into());
        assert_eq!(
            diff_lines(&old, &new),
            vec!["Título da spec alterado", "test_command alterado", "Critérios globais: 1 novo"]
        );
    }

    #[test]
    fn limit_is_twenty_lines_with_ellipsis() {
        let old = plan(Vec::new());
        let new = plan((1..=30).map(|n| task(&format!("T{n:02}"), "x", &[])).collect());
        let lines = diff_lines(&old, &new);
        assert_eq!(lines.len(), MAX_CHANGE_LINES);
        assert_eq!(lines.last().map(String::as_str), Some(MORE_LINE));
        assert_eq!(lines[0], "T01 nova: x");
        let exact = plan((1..=20).map(|n| task(&format!("T{n:02}"), "x", &[])).collect());
        assert!(diff_lines(&old, &exact).iter().all(|l| l != MORE_LINE), "20 cabem sem corte");
    }

    #[test]
    fn long_title_is_cut() {
        let new = plan(vec![task("T01", &"a".repeat(200), &[])]);
        let line = &diff_lines(&plan(Vec::new()), &new)[0];
        assert!(line.ends_with('…') && line.chars().count() < 100, "{line}");
    }
}
