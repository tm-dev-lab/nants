//! Aviso de custo por tarefa (spec 124 T06): um SINAL, nunca um limite.
//!
//! O gasto de cada tarefa vem do `usage.jsonl` da spec, somado pelo [`task_cost_of`] da
//! recuperação (spec 124 T03): tokens (entrada mais saída) e chamadas, de todos os papéis.
//!
//! A tarefa passa do patamar quando:
//! - **relativo:** com ao menos [`MIN_DONE_FOR_MEDIAN`] outras tarefas concluídas (DONE) na spec,
//!   os tokens ou as chamadas chegam a [`RELATIVE_FACTOR`] vezes a mediana delas, e também ao piso
//!   ([`RELATIVE_FLOOR_TOKENS`] ou [`RELATIVE_FLOOR_CALLS`]) para uma spec de tarefas minúsculas não
//!   acender o aviso por centavos;
//! - **absoluto:** os tokens chegam a [`ABSOLUTE_TOKENS`] ou as chamadas a [`ABSOLUTE_CALLS`], com
//!   ou sem histórico na spec (no Test 3, a T05 chegou a 16,2 M de tokens de entrada e 637 chamadas
//!   sem nenhum sinal; o patamar absoluto acende na metade disso).
//!
//! O aviso vai ao Arquiteto na próxima decisão (os FATOS da recuperação e da arbitragem) e à tela
//! (Quadro e Atividade, pelo estado do projeto). Nada aqui muda estado, tentativa ou fluxo.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::SystemTime;

use serde::Serialize;

use super::recovery::task_cost_of;
use super::safe_fs::{self, MAX_READ_BYTES};
use crate::agents::recovery_plan::RoleCost;
use crate::sdd::{Task, TaskStatus};

/// Quantas vezes a mediana das concluídas acende o aviso relativo.
pub const RELATIVE_FACTOR: u64 = 3;
/// Concluídas necessárias para a mediana valer (com uma só, não há distribuição para comparar).
pub const MIN_DONE_FOR_MEDIAN: usize = 2;
/// Piso do aviso relativo, em tokens.
pub const RELATIVE_FLOOR_TOKENS: u64 = 1_000_000;
/// Piso do aviso relativo, em chamadas.
pub const RELATIVE_FLOOR_CALLS: u64 = 100;
/// Patamar absoluto, em tokens.
pub const ABSOLUTE_TOKENS: u64 = 8_000_000;
/// Patamar absoluto, em chamadas.
pub const ABSOLUTE_CALLS: u64 = 400;

const SPECS_REL: &str = ".sdd/specs";
/// Entradas do cache por arquivo de uso (limpo inteiro ao encher).
const CACHE_MAX_ENTRIES: usize = 256;

/// Gasto de uma tarefa até aqui, somado entre os papéis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spend {
    pub tokens: u64,
    pub calls: u64,
}

impl Spend {
    pub fn of(costs: &[RoleCost]) -> Self {
        costs.iter().fold(Self::default(), |acc, c| Self {
            tokens: acc.tokens.saturating_add(c.input).saturating_add(c.output),
            calls: acc.calls.saturating_add(c.calls),
        })
    }
}

/// Qual patamar acendeu o aviso.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Relative,
    Absolute,
}

/// Aviso de custo de uma tarefa (exposto no estado do projeto como `cost_alerts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CostAlert {
    pub tokens: u64,
    pub calls: u64,
    pub basis: Basis,
    /// Vezes a mediana (arredondado para baixo), só no relativo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<u64>,
    /// Mediana das concluídas da spec, quando há concluídas suficientes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_calls: Option<u64>,
}

/// Mediana (média dos dois do meio quando a quantidade é par).
fn median(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    let n = values.len();
    match n {
        0 => 0,
        _ if n % 2 == 1 => values[n / 2],
        _ => u64::try_from((u128::from(values[n / 2 - 1]) + u128::from(values[n / 2])) / 2).unwrap_or(u64::MAX),
    }
}

/// Avalia uma tarefa contra as concluídas da spec (sem ela própria). `None`: abaixo do patamar.
pub fn evaluate(spend: Spend, done: &[Spend]) -> Option<CostAlert> {
    let medians = (done.len() >= MIN_DONE_FOR_MEDIAN).then(|| {
        let mut tokens: Vec<u64> = done.iter().map(|s| s.tokens).collect();
        let mut calls: Vec<u64> = done.iter().map(|s| s.calls).collect();
        (median(&mut tokens), median(&mut calls))
    });
    let over = |value: u64, median: u64, floor: u64| median > 0 && value >= median.saturating_mul(RELATIVE_FACTOR) && value >= floor;
    let ratio = medians.and_then(|(mt, mc)| {
        let by_tokens = over(spend.tokens, mt, RELATIVE_FLOOR_TOKENS).then(|| spend.tokens / mt);
        let by_calls = over(spend.calls, mc, RELATIVE_FLOOR_CALLS).then(|| spend.calls / mc);
        by_tokens.max(by_calls)
    });
    let basis = if ratio.is_some() {
        Basis::Relative
    } else if spend.tokens >= ABSOLUTE_TOKENS || spend.calls >= ABSOLUTE_CALLS {
        Basis::Absolute
    } else {
        return None;
    };
    Some(CostAlert {
        tokens: spend.tokens,
        calls: spend.calls,
        basis,
        ratio,
        median_tokens: medians.map(|(t, _)| t),
        median_calls: medians.map(|(_, c)| c),
    })
}

/// Avisos das tarefas `(id, concluída)` de uma spec, a partir do gasto por tarefa.
pub fn alerts(spend: &BTreeMap<String, Spend>, tasks: &[(String, bool)]) -> BTreeMap<String, CostAlert> {
    let mut out = BTreeMap::new();
    for (id, _) in tasks {
        let Some(own) = spend.get(id) else { continue };
        let done: Vec<Spend> = tasks
            .iter()
            .filter(|(other, is_done)| *is_done && other != id)
            .filter_map(|(other, _)| spend.get(other).copied())
            .collect();
        if let Some(alert) = evaluate(*own, &done) {
            out.insert(id.clone(), alert);
        }
    }
    out
}

type Signature = (u64, Option<SystemTime>);

fn cache() -> &'static Mutex<HashMap<PathBuf, (Signature, BTreeMap<String, Spend>)>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, (Signature, BTreeMap<String, Spend>)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Gasto por tarefa de uma spec (`usage.jsonl`). Ausente ou inseguro: vazio. Relê só quando o
/// tamanho ou o mtime do arquivo mudam (o estado do projeto é recarregado com frequência).
pub fn spend_by_task(root: &Path, spec_id: &str) -> BTreeMap<String, Spend> {
    let Ok(path) = safe_fs::check_chain(root, &format!("{SPECS_REL}/{spec_id}/usage.jsonl")) else {
        return BTreeMap::new();
    };
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return BTreeMap::new();
    };
    if !meta.file_type().is_file() {
        return BTreeMap::new();
    }
    let signature: Signature = (meta.len(), meta.modified().ok());
    {
        let guard = cache().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((sig, hit)) = guard.get(&path) {
            if *sig == signature {
                return hit.clone();
            }
        }
    }
    let records = super::usage::load_records(root, Some(spec_id));
    let mut ids: Vec<&str> = records.iter().filter_map(|r| r.task.as_deref()).collect();
    ids.sort_unstable();
    ids.dedup();
    let spend: BTreeMap<String, Spend> = ids.iter().map(|id| ((*id).to_string(), Spend::of(&task_cost_of(&records, id)))).collect();
    let mut guard = cache().lock().unwrap_or_else(PoisonError::into_inner);
    if guard.len() >= CACHE_MAX_ENTRIES {
        guard.clear();
    }
    guard.insert(path, (signature, spend.clone()));
    spend
}

/// Tarefas `(id, concluída)` lidas de `.sdd/specs/<spec>/tasks/*.md`; ilegíveis ficam de fora.
fn task_states(root: &Path, spec_id: &str) -> Vec<(String, bool)> {
    super::plan_review::task_names(root, spec_id)
        .iter()
        .filter_map(|name| safe_fs::read_regular(root, &format!("{SPECS_REL}/{spec_id}/tasks/{name}"), MAX_READ_BYTES).ok())
        .filter_map(|raw| Task::parse(&raw).ok())
        .map(|t| (t.frontmatter.id, t.frontmatter.status == TaskStatus::Done))
        .collect()
}

/// Aviso de uma tarefa, lendo o `.sdd` e o uso da spec em `root`.
pub fn task_alert(root: &Path, spec_id: &str, task_id: &str) -> Option<CostAlert> {
    let spend = spend_by_task(root, spec_id);
    spend.get(task_id)?;
    alerts(&spend, &task_states(root, spec_id)).remove(task_id)
}

/// Tarefas ainda não concluídas com aviso de custo nas specs em andamento do projeto em `root`
/// (spec 138 T02: o observador de custo do Nanty). Só leitura; pastas ilegíveis contam zero.
pub fn open_alert_count(root: &Path) -> usize {
    if safe_fs::check_chain(root, SPECS_REL).is_err() {
        return 0;
    }
    super::projects::real_children(&root.join(SPECS_REL), true)
        .iter()
        .filter(|name| !name.starts_with('.'))
        .filter(|name| {
            super::projects::read_spec(root, &format!("{SPECS_REL}/{name}/spec.md"))
                .is_ok_and(|spec| spec.frontmatter.status == crate::sdd::SpecStatus::InProgress)
        })
        .map(|name| {
            let tasks = task_states(root, name);
            alerts(&spend_by_task(root, name), &tasks)
                .keys()
                .filter(|id| tasks.iter().any(|(t, done)| t == *id && !done))
                .count()
        })
        .sum()
}

/// Linha dos FATOS do Arquiteto (texto de prompt, não de pessoa).
pub fn architect_line(alert: &CostAlert) -> String {
    let basis = match (alert.basis, alert.ratio, alert.median_tokens, alert.median_calls) {
        (Basis::Relative, Some(ratio), Some(mt), Some(mc)) => format!(
            "{ratio} vezes a mediana das tarefas concluídas da spec ({mt} tokens, {mc} chamadas; patamar relativo: {RELATIVE_FACTOR} vezes)"
        ),
        _ => format!("patamar absoluto: {ABSOLUTE_TOKENS} tokens ou {ABSOLUTE_CALLS} chamadas"),
    };
    format!(
        "- Aviso de custo: a tarefa já usou {} tokens em {} {}, {basis}. É um sinal, não um limite: a execução continua; \
         considere SPLIT ou ADJUST se a tarefa estiver grande demais.",
        alert.tokens,
        alert.calls,
        if alert.calls == 1 { "chamada" } else { "chamadas" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(tokens: u64, calls: u64) -> Spend {
        Spend { tokens, calls }
    }

    #[test]
    fn median_handles_odd_even_and_empty() {
        assert_eq!(median(&mut []), 0);
        assert_eq!(median(&mut [5, 1, 3]), 3);
        assert_eq!(median(&mut [4, 1, 3, 2]), 2);
        assert_eq!(median(&mut [u64::MAX, u64::MAX]), u64::MAX);
    }

    #[test]
    fn relative_threshold_is_three_times_the_median_with_a_floor() {
        let done = [s(1_000_000, 100), s(2_000_000, 150), s(3_000_000, 200)];
        assert_eq!(evaluate(s(5_999_999, 399), &done), None, "abaixo de 3x a mediana");
        let at = evaluate(s(6_000_000, 120), &done).expect("3x a mediana de tokens");
        assert_eq!((at.basis, at.ratio, at.median_tokens, at.median_calls), (Basis::Relative, Some(3), Some(2_000_000), Some(150)));
        let calls = evaluate(s(10, 600), &done).expect("4x a mediana de chamadas");
        assert_eq!((calls.basis, calls.ratio), (Basis::Relative, Some(4)));
        // Tarefas minúsculas: 3x a mediana não basta abaixo do piso.
        let tiny = [s(10_000, 3), s(20_000, 4)];
        assert_eq!(evaluate(s(900_000, 90), &tiny), None);
        assert_eq!(evaluate(s(RELATIVE_FLOOR_TOKENS, 5), &tiny).map(|a| a.basis), Some(Basis::Relative));
    }

    #[test]
    fn median_needs_two_done_tasks_and_absolute_works_without_history() {
        assert_eq!(evaluate(s(7_999_999, 399), &[]), None);
        assert_eq!(evaluate(s(7_999_999, 399), &[s(1_000, 1)]), None, "uma concluída não forma mediana");
        let a = evaluate(s(ABSOLUTE_TOKENS, 1), &[]).expect("absoluto em tokens");
        assert_eq!((a.basis, a.ratio, a.median_tokens), (Basis::Absolute, None, None));
        assert_eq!(evaluate(s(1, ABSOLUTE_CALLS), &[s(1_000, 1)]).map(|a| a.basis), Some(Basis::Absolute));
        // Test 3, T05: 16,2 M de tokens e 637 chamadas.
        assert!(evaluate(s(16_290_000, 637), &[]).is_some());
    }

    #[test]
    fn alerts_compare_against_other_done_tasks_only() {
        let spend = BTreeMap::from([
            ("T01".to_string(), s(1_000_000, 100)),
            ("T02".to_string(), s(1_200_000, 110)),
            ("T03".to_string(), s(5_000_000, 300)),
            ("T04".to_string(), s(9_000_000, 50)),
        ]);
        let tasks = vec![
            ("T01".to_string(), true),
            ("T02".to_string(), true),
            ("T03".to_string(), false),
            ("T04".to_string(), false),
            ("T05".to_string(), false),
        ];
        let out = alerts(&spend, &tasks);
        assert_eq!(out.keys().collect::<Vec<_>>(), vec!["T03", "T04"]);
        assert_eq!(out["T03"].basis, Basis::Relative);
        assert_eq!(out["T03"].median_tokens, Some(1_100_000));
        assert_eq!(out["T04"].ratio, Some(8));
    }

    #[test]
    fn architect_line_says_signal_not_limit() {
        let rel = evaluate(s(6_000_000, 120), &[s(1_000_000, 100), s(2_000_000, 150), s(3_000_000, 200)]).expect("relativo");
        let line = architect_line(&rel);
        assert!(line.starts_with("- Aviso de custo: a tarefa já usou 6000000 tokens em 120 chamadas, 3 vezes a mediana"), "{line}");
        assert!(line.contains("não um limite") && line.contains("SPLIT ou ADJUST"), "{line}");
        let abs = architect_line(&evaluate(s(9_000_000, 10), &[]).expect("absoluto"));
        assert!(abs.contains("patamar absoluto: 8000000 tokens ou 400 chamadas"), "{abs}");
    }

    #[test]
    fn serializes_for_the_project_state() {
        let a = evaluate(s(9_000_000, 10), &[]).expect("absoluto");
        assert_eq!(serde_json::to_value(&a).expect("json"), serde_json::json!({"tokens": 9_000_000, "calls": 10, "basis": "absolute"}));
    }

    const SPEC: &str = "001-custo";
    const PROJECT: &str = "proj-custo";

    fn task_md(id: &str, status: &str) -> String {
        format!(
            "---\nid: \"{id}\"\nspec_id: \"{SPEC}\"\ntitle: \"t\"\nstatus: \"{status}\"\ndepends_on: []\nallowed_scope: [\"src/**\"]\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\n---\n\n## Objetivo\nx\n"
        )
    }

    fn usage_line(task: &str, input: u64, n: usize) -> String {
        format!(
            "{{\"at\":\"2026-10-24T10:00:00Z\",\"project\":\"p\",\"spec\":\"{SPEC}\",\"task\":\"{task}\",\"agent\":\"developer\",\"model\":\"m\",\"input\":{input},\"output\":0}}\n"
        )
        .repeat(n)
    }

    fn project(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nants-cost-alert-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join(SPECS_REL).join(SPEC);
        fs::create_dir_all(dir.join("tasks")).expect("tasks");
        fs::write(dir.join("spec.md"), format!("---\nid: \"{SPEC}\"\ntitle: \"Custo\"\nstatus: \"IN_PROGRESS\"\n---\n")).expect("spec");
        for (id, status) in [("T01", "DONE"), ("T02", "DONE"), ("T03", "IN_PROGRESS"), ("T04", "TODO")] {
            fs::write(dir.join(format!("tasks/{id}.md")), task_md(id, status)).expect("tarefa");
        }
        root
    }

    /// O gasto relido acompanha o arquivo de uso (cache por tamanho e mtime) e o aviso só olha as
    /// concluídas; nada muda no arquivo da tarefa.
    #[test]
    fn task_alert_reads_usage_and_states_and_never_touches_the_task() {
        let root = project("task");
        let usage = root.join(SPECS_REL).join(SPEC).join("usage.jsonl");
        assert_eq!(task_alert(&root, SPEC, "T03"), None, "sem uso");
        fs::write(&usage, [usage_line("T01", 400_000, 1), usage_line("T02", 600_000, 1), usage_line("T03", 900_000, 1)].concat()).expect("uso");
        assert_eq!(task_alert(&root, SPEC, "T03"), None, "1,8x a mediana");
        let before = fs::read_to_string(root.join(SPECS_REL).join(SPEC).join("tasks/T03.md")).expect("ler");
        fs::write(&usage, [usage_line("T01", 400_000, 1), usage_line("T02", 600_000, 1), usage_line("T03", 1_600_000, 1)].concat()).expect("uso");
        let alert = task_alert(&root, SPEC, "T03").expect("3x a mediana (500 mil) e acima do piso");
        assert_eq!((alert.basis, alert.ratio, alert.median_tokens), (Basis::Relative, Some(3), Some(500_000)));
        let after = fs::read_to_string(root.join(SPECS_REL).join(SPEC).join("tasks/T03.md")).expect("ler");
        assert_eq!(before, after, "o aviso não muda a tarefa");
        let _ = fs::remove_dir_all(&root);
    }

    /// O estado do projeto expõe `cost_alerts` por tarefa, só as acima do patamar.
    #[tokio::test]
    async fn project_state_exposes_cost_alerts() {
        use axum::body::{to_bytes, Body};
        use axum::http::{header, Method, Request, StatusCode};
        use tower::ServiceExt;
        let root = project("state");
        fs::write(root.join(".sdd/settings.yaml"), "base_branch: main\ntest_command: \"true\"\n").expect("settings");
        let usage = [usage_line("T01", 10_000, 10), usage_line("T02", 10_000, 12), usage_line("T03", 10_000, ABSOLUTE_CALLS as usize), usage_line("T04", 10_000, 1)].concat();
        fs::write(root.join(SPECS_REL).join(SPEC).join("usage.jsonl"), usage).expect("uso");
        let mut cfg = crate::config::Config::default();
        cfg.projects.push(crate::config::Project {
            id: PROJECT.into(),
            name: "Custo".into(),
            path: root.display().to_string(),
            created_at: "2026-10-24T00:00:00Z".into(),
        });
        let state = super::super::AppState::new(root.join("config.toml"), cfg, 3000);
        let req = Request::builder()
            .method(Method::GET)
            .uri(format!("/api/projects/{PROJECT}/state"))
            .header(header::HOST, "127.0.0.1:3000")
            .body(Body::empty())
            .expect("requisição");
        let resp = super::super::router(state).oneshot(req).await.expect("resposta");
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.expect("corpo");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let alerts = &body["specs"][0]["cost_alerts"];
        assert_eq!(alerts.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()), Some(vec!["T03".to_string()]), "{body}");
        assert_eq!(alerts["T03"]["basis"], "relative");
        assert_eq!(alerts["T03"]["calls"], ABSOLUTE_CALLS);
        assert_eq!(alerts["T03"]["median_calls"], 11);
        let _ = fs::remove_dir_all(&root);
    }
}
