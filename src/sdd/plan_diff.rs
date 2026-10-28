//! "O que mudou no plano" entre versões (spec 086 T09). Funções puras e determinísticas, sem
//! modelo: o servidor monta um [`PlanSnapshot`] antes do turno do chat e outro depois, e
//! [`diff_lines`] descreve a diferença em linhas curtas ("T04 nova: <título>", "T02 removida",
//! "T03: critérios alterados (2 novos, 1 removido)", "test_command alterado"…).
//!
//! Desde a spec 131 T06 cada linha é um [`ChangeLine`]: `code` + `params`, sem idioma. A web a
//! traduz (`planChange.<code>`) e o servidor a escreve no idioma do projeto quando a cita ao modelo
//! (`plan_review.change.<code>`).
//!
//! Só conteúdo conta: espaços, marcadores de lista, caixas `[ ]` e linhas em branco são
//! normalizados, e listas de `depends_on`/`allowed_scope`/`covers` são comparadas sem ordem.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::coverage::global_criteria;
use super::{Spec, Task};
use crate::agents::runner::{section_text, ACCEPTANCE};
use crate::i18n::{tr, tr_plural, Lang};

/// Máximo de linhas do resumo (incluindo a de [`CODE_MORE`] quando há mais).
pub const MAX_CHANGE_LINES: usize = 20;

/// Códigos das linhas (a chave é `plan_review.change.<código>` no servidor e `planChange.<código>`
/// na web).
pub const CODE_SPEC_TITLE: &str = "spec_title";
pub const CODE_TEST_COMMAND: &str = "test_command";
pub const CODE_GLOBAL_CRITERIA: &str = "global_criteria";
pub const CODE_TASK_REMOVED: &str = "task_removed";
pub const CODE_TASK_NEW: &str = "task_new";
pub const CODE_TASK_TITLE: &str = "task_title";
pub const CODE_TASK_DEPENDS: &str = "task_depends";
pub const CODE_TASK_SCOPE: &str = "task_scope";
pub const CODE_TASK_CRITERIA: &str = "task_criteria";
pub const CODE_TASK_COVERS: &str = "task_covers";
/// Última linha quando o resumo é cortado.
pub const CODE_MORE: &str = "more";
/// Prefixo das chaves das linhas no dicionário do servidor.
pub const CHANGE_KEY_PREFIX: &str = "plan_review.change.";

/// Uma linha do resumo, neutra de idioma: `params` traz `id` (tarefa), `title` (tarefa nova) e, nas
/// listas, `added` e `removed` (a contagem; os dois em zero = só reordenados).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeLine {
    pub code: String,
    #[serde(default)]
    pub params: Map<String, Value>,
}

impl ChangeLine {
    fn new(code: &str, params: &[(&str, Value)]) -> Self {
        Self { code: code.to_string(), params: params.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect() }
    }

    fn task(code: &str, id: &str) -> Self {
        Self::new(code, &[("id", Value::from(id))])
    }

    fn count(&self, name: &str) -> u64 {
        self.params.get(name).and_then(Value::as_u64).unwrap_or(0)
    }

    /// Texto da linha no idioma (para o modelo e para os testes); a web monta o mesmo texto.
    pub fn render(&self, lang: Lang) -> String {
        let mut values: Vec<(String, String)> = self
            .params
            .iter()
            .filter_map(|(k, v)| match v {
                Value::String(s) => Some((k.clone(), s.clone())),
                Value::Number(n) => Some((k.clone(), n.to_string())),
                _ => None,
            })
            .collect();
        if self.code == CODE_GLOBAL_CRITERIA || self.code == CODE_TASK_CRITERIA {
            let (added, removed) = (self.count("added"), self.count("removed"));
            let mut parts = Vec::new();
            if added > 0 {
                parts.push(tr_plural(lang, &format!("{CHANGE_KEY_PREFIX}added"), added, &[]));
            }
            if removed > 0 {
                parts.push(tr_plural(lang, &format!("{CHANGE_KEY_PREFIX}removed"), removed, &[]));
            }
            let delta = if parts.is_empty() { tr(lang, &format!("{CHANGE_KEY_PREFIX}reordered"), &[]) } else { parts.join(", ") };
            values.push(("delta".to_string(), delta));
        }
        let pairs: Vec<(&str, &str)> = values.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        tr(lang, &format!("{CHANGE_KEY_PREFIX}{}", self.code), &pairs)
    }
}
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

/// Contagem da mudança de uma lista (`added`, `removed`); `None` sem mudança. Os mesmos itens em
/// outra ordem dão os dois em zero ("reordenados").
fn list_delta(old: &[String], new: &[String]) -> Option<(usize, usize)> {
    if old == new {
        return None;
    }
    Some(added_removed(old, new))
}

/// Linha de uma lista alterada, com a contagem.
fn delta_line(code: &str, id: Option<&str>, (added, removed): (usize, usize)) -> ChangeLine {
    let mut line = ChangeLine::new(code, &[("added", Value::from(added)), ("removed", Value::from(removed))]);
    if let Some(id) = id {
        line.params.insert("id".into(), Value::from(id));
    }
    line
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
/// [`MAX_CHANGE_LINES`] linhas; se houver mais, a última é [`CODE_MORE`].
pub fn diff_lines(old: &PlanSnapshot, new: &PlanSnapshot) -> Vec<ChangeLine> {
    let mut lines = Vec::new();
    if old.title != new.title {
        lines.push(ChangeLine::new(CODE_SPEC_TITLE, &[]));
    }
    if old.test_command != new.test_command {
        lines.push(ChangeLine::new(CODE_TEST_COMMAND, &[]));
    }
    if let Some(delta) = list_delta(&old.global_criteria, &new.global_criteria) {
        lines.push(delta_line(CODE_GLOBAL_CRITERIA, None, delta));
    }
    let old_tasks: BTreeMap<&str, &TaskSnapshot> = old.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let new_tasks: BTreeMap<&str, &TaskSnapshot> = new.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut ids: Vec<&str> = old_tasks.keys().chain(new_tasks.keys()).copied().collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        match (old_tasks.get(id), new_tasks.get(id)) {
            (Some(_), None) => lines.push(ChangeLine::task(CODE_TASK_REMOVED, id)),
            (None, Some(t)) => lines.push(ChangeLine::new(CODE_TASK_NEW, &[("id", Value::from(id)), ("title", Value::from(short_title(&t.title)))])),
            (Some(a), Some(b)) => task_lines(id, a, b, &mut lines),
            (None, None) => {}
        }
    }
    if lines.len() > MAX_CHANGE_LINES {
        lines.truncate(MAX_CHANGE_LINES - 1);
        lines.push(ChangeLine::new(CODE_MORE, &[]));
    }
    lines
}

fn task_lines(id: &str, a: &TaskSnapshot, b: &TaskSnapshot, lines: &mut Vec<ChangeLine>) {
    if a.title != b.title {
        lines.push(ChangeLine::task(CODE_TASK_TITLE, id));
    }
    if a.depends_on != b.depends_on {
        lines.push(ChangeLine::task(CODE_TASK_DEPENDS, id));
    }
    if a.allowed_scope != b.allowed_scope {
        lines.push(ChangeLine::task(CODE_TASK_SCOPE, id));
    }
    if let Some(delta) = list_delta(&a.criteria, &b.criteria) {
        lines.push(delta_line(CODE_TASK_CRITERIA, Some(id), delta));
    }
    if a.covers != b.covers {
        lines.push(ChangeLine::task(CODE_TASK_COVERS, id));
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

    /// Linhas em pt-BR (o texto de antes da spec 131 T06).
    fn pt(old: &PlanSnapshot, new: &PlanSnapshot) -> Vec<String> {
        diff_lines(old, new).iter().map(|l| l.render(Lang::PtBr)).collect()
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
        assert_eq!(pt(&old, &new), vec!["T02 removida", "T04 nova: Nova tarefa"]);
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
            pt(&old, &new),
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
        assert_eq!(pt(&plan(vec![a]), &plan(vec![b])), vec!["T01: critérios alterados (reordenados)"]);
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
            pt(&old, &new),
            vec!["Título da spec alterado", "test_command alterado", "Critérios globais: 1 novo"]
        );
    }

    #[test]
    fn limit_is_twenty_lines_with_ellipsis() {
        let old = plan(Vec::new());
        let new = plan((1..=30).map(|n| task(&format!("T{n:02}"), "x", &[])).collect());
        let lines = diff_lines(&old, &new);
        assert_eq!(lines.len(), MAX_CHANGE_LINES);
        assert_eq!(lines.last().map(|l| l.code.as_str()), Some(CODE_MORE));
        assert_eq!(lines.last().map(|l| l.render(Lang::En)).as_deref(), Some("and more changes beyond these"));
        assert_eq!(lines[0].render(Lang::PtBr), "T01 nova: x");
        let exact = plan((1..=20).map(|n| task(&format!("T{n:02}"), "x", &[])).collect());
        assert!(diff_lines(&old, &exact).iter().all(|l| l.code != CODE_MORE), "20 cabem sem corte");
    }

    #[test]
    fn long_title_is_cut() {
        let new = plan(vec![task("T01", &"a".repeat(200), &[])]);
        let line = &pt(&plan(Vec::new()), &new)[0];
        assert!(line.ends_with('…') && line.chars().count() < 100, "{line}");
    }

    /// Spec 131 T06: a linha sai neutra (`code` + `params`) e o texto segue o idioma pedido.
    #[test]
    fn lines_are_coded_and_render_in_both_languages() {
        let old = plan(vec![task("T06", "A", &["um"]), task("T07", "B", &["um"])]);
        let mut t07 = task("T07", "B", &[]);
        t07.depends_on = vec!["T06".into()];
        t07.allowed_scope = vec!["web/**".into()];
        let new = plan(vec![task("T06", "A", &["um"]), t07, task("T08", "Executable launcher", &[])]);
        let lines = diff_lines(&old, &new);
        assert_eq!(lines[0].code, CODE_TASK_DEPENDS);
        assert_eq!(lines[0].params["id"], "T07");
        assert_eq!(lines[2].code, CODE_TASK_CRITERIA);
        assert_eq!((lines[2].params["added"].as_u64(), lines[2].params["removed"].as_u64()), (Some(0), Some(1)));
        let en: Vec<String> = lines.iter().map(|l| l.render(Lang::En)).collect();
        assert_eq!(
            en,
            vec![
                "T07: dependencies changed",
                "T07: scope changed",
                "T07: criteria changed (1 removed)",
                "T08 new: Executable launcher",
            ]
        );
        assert_eq!(
            pt(&old, &new),
            vec!["T07: dependências alteradas", "T07: escopo alterado", "T07: critérios alterados (1 removido)", "T08 nova: Executable launcher"]
        );
        let json = serde_json::to_string(&lines[0]).expect("json");
        assert_eq!(serde_json::from_str::<ChangeLine>(&json).expect("ler"), lines[0]);
    }
}
