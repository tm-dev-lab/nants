//! Reconciliador SDD x Git (BLUEPRINT §5.3).
//!
//! Cruza o estado SDD (`.sdd/specs/**/tasks/*.md`) com a visão Git (branches, worktrees e
//! commits da base) e produz ações corretivas. `reconcile` é pura: não executa nada.
//!
//! Ordem das ações: por spec e por tarefa, na ordem de carga. Para uma tarefa integrada, a
//! sequência é `MarkDone`, `CleanWorktree*` e por fim `DeleteBranch` (a worktree precisa sair
//! antes para que `git branch -D` funcione).
//!
//! Regras de segurança: commits casam por prefixo exato `feat(<spec>): <task> - ` (reverts não
//! contam); `DeleteBranch` nunca atinge a base nem a branch da worktree principal; órfãs são
//! apenas worktrees com branch `sdd/*` não reivindicada por tarefa não-DONE (inclusive o nome
//! derivado `sdd/<spec>/<task>`); a worktree principal nunca é removida.
//!
//! Fail-closed: com `base_branch` vazio, tarefas INTEGRATING/DONE são apenas sinalizadas e a
//! limpeza de órfãs é suspensa. Worktrees com alterações não commitadas (`dirty_worktrees`)
//! nunca são removidas: viram `FlagDivergence`, e a branch delas não é apagada.
//!
//! Idempotência: `MarkDone` e `ReopenIntegrating` mudam o estado SDD; `CleanWorktree` e
//! `DeleteBranch` mudam o estado Git. Depois de aplicá-las, a tarefa deixa de casar nas regras
//! que as geraram e uma nova reconciliação não as repete. `ResumeInProgress` e
//! `FlagDivergence` são informativas: são reemitidas enquanto a situação persistir, e cabe ao
//! chamador apresentá-las (ex.: retomar a worktree existente ou pedir intervenção humana).

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use super::{Sdd, TaskStatus};
use crate::harness::git::{Git, WorktreeInfo};
use crate::harness::GitError;

/// Visão do Git usada na reconciliação.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitView {
    /// Branches locais.
    pub branches: Vec<String>,
    /// Worktrees; a primeira é sempre a worktree principal (raiz do repositório).
    pub worktrees: Vec<WorktreeInfo>,
    /// Assuntos dos commits recentes da branch base.
    pub base_subjects: Vec<String>,
    /// Caminhos das worktrees secundárias com alterações não commitadas.
    pub dirty_worktrees: Vec<String>,
}

/// Motivo usado quando uma worktree com alterações não commitadas não pode ser removida.
const DIRTY_REASON: &str = "worktree com alterações não commitadas";

/// Ação sugerida pelo reconciliador.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Marca a tarefa como `DONE` (o squash já consta na base).
    MarkDone { spec_id: String, task_id: String },
    /// Devolve a tarefa `INTEGRATING` para `APPROVED` (squash ausente da base).
    ReopenIntegrating { spec_id: String, task_id: String },
    /// Inconsistência que exige atenção; não é corrigida automaticamente.
    FlagDivergence {
        spec_id: String,
        task_id: String,
        reason: String,
    },
    /// Remove a worktree em `path` (nunca a principal).
    CleanWorktree { path: String },
    /// Retoma a tarefa usando a worktree/branch existente.
    ResumeInProgress {
        spec_id: String,
        task_id: String,
        branch: String,
    },
    /// Remove a branch `branch` (já integrada).
    DeleteBranch { branch: String },
}

/// Prefixo das branches de tarefa criadas pelo harness. Só branches com este prefixo sem
/// tarefa ativa são consideradas órfãs (`worktree-*` pode pertencer a agente em execução).
const TASK_BRANCH_PREFIX: &str = "sdd/";

/// `true` quando `subject` é o commit de squash da tarefa: começa com `feat(<spec>): <task> - `.
/// Reverts (`Revert "feat(...)..."`) e outros commits que apenas citam o assunto não casam.
pub(crate) fn is_task_commit(subject: &str, spec_id: &str, task_id: &str) -> bool {
    subject.starts_with(&format!("feat({spec_id}): {task_id} - "))
}

/// Calcula as ações de reconciliação. Função pura e determinística.
pub fn reconcile(sdd: &Sdd, view: &GitView) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut handled: HashSet<PathBuf> = HashSet::new();
    // Branches de tarefas não-DONE: nunca são tratadas como órfãs.
    // Inclui também o nome derivado `sdd/<spec>/<task>`, que vale mesmo com `branch` vazia.
    let mut owned: BTreeSet<String> = BTreeSet::new();
    // Nunca apaga a base nem a branch da worktree principal.
    let root_branch = view.worktrees.first().and_then(|w| w.branch.as_deref());
    let base_branch = sdd.settings.base_branch.as_str();
    // Fail-closed: sem base não há como provar integração, então nada é limpo.
    let base_empty = base_branch.is_empty();

    for (spec, tasks) in &sdd.specs {
        for task in tasks {
            let meta = &task.frontmatter;
            if meta.status != TaskStatus::Done {
                if !meta.branch.is_empty() {
                    owned.insert(meta.branch.clone());
                }
                owned.insert(format!(
                    "{TASK_BRANCH_PREFIX}{}/{}",
                    spec.frontmatter.id, meta.id
                ));
            }
        }
    }

    for (spec, tasks) in &sdd.specs {
        let spec_id = spec.frontmatter.id.as_str();
        for task in tasks {
            let meta = &task.frontmatter;
            let task_id = meta.id.as_str();
            let branch = meta.branch.as_str();
            let has_commit = view
                .base_subjects
                .iter()
                .any(|s| is_task_commit(s, spec_id, task_id));
            let branch_exists = !branch.is_empty() && view.branches.iter().any(|b| b == branch);
            let branch_dirty = view
                .worktrees
                .iter()
                .enumerate()
                .any(|(i, w)| i > 0 && w.branch.as_deref() == Some(branch) && is_dirty(view, w));

            match meta.status {
                TaskStatus::Integrating | TaskStatus::Done if base_empty => {
                    actions.push(Action::FlagDivergence {
                        spec_id: spec_id.to_string(),
                        task_id: task_id.to_string(),
                        reason: "base_branch vazio".to_string(),
                    });
                }
                TaskStatus::Integrating if has_commit => {
                    actions.push(Action::MarkDone {
                        spec_id: spec_id.to_string(),
                        task_id: task_id.to_string(),
                    });
                    for (i, wt) in view.worktrees.iter().enumerate() {
                        if i > 0 && wt.branch.as_deref() == Some(branch) {
                            clean_or_flag(&mut actions, &mut handled, view, wt, spec_id, task_id);
                        }
                    }
                    if branch_exists
                        && !branch_dirty
                        && Some(branch) != root_branch
                        && branch != base_branch
                    {
                        actions.push(Action::DeleteBranch {
                            branch: branch.to_string(),
                        });
                    }
                }
                TaskStatus::Integrating => {
                    actions.push(Action::ReopenIntegrating {
                        spec_id: spec_id.to_string(),
                        task_id: task_id.to_string(),
                    });
                }
                TaskStatus::Done if !has_commit => {
                    actions.push(Action::FlagDivergence {
                        spec_id: spec_id.to_string(),
                        task_id: task_id.to_string(),
                        reason: format!(
                            "status DONE sem commit `feat({spec_id}): {task_id} - ...` na base"
                        ),
                    });
                }
                TaskStatus::InProgress | TaskStatus::InReview | TaskStatus::ChangesRequested
                    if !branch.is_empty() =>
                {
                    if branch_exists {
                        actions.push(Action::ResumeInProgress {
                            spec_id: spec_id.to_string(),
                            task_id: task_id.to_string(),
                            branch: branch.to_string(),
                        });
                    } else {
                        actions.push(Action::FlagDivergence {
                            spec_id: spec_id.to_string(),
                            task_id: task_id.to_string(),
                            reason: format!("branch ausente: {branch}"),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // Regra 5: worktrees órfãs. A principal (índice 0) nunca é candidata.
    if !base_empty {
        for (i, wt) in view.worktrees.iter().enumerate() {
            if i == 0 {
                continue;
            }
            let Some(branch) = wt.branch.as_deref() else {
                continue;
            };
            if branch.starts_with(TASK_BRANCH_PREFIX) && !owned.contains(branch) {
                let (spec_id, task_id) = owner_of(branch);
                clean_or_flag(&mut actions, &mut handled, view, wt, &spec_id, &task_id);
            }
        }
    }

    actions
}

/// `true` quando a worktree `wt` está listada como suja em `view`.
fn is_dirty(view: &GitView, wt: &WorktreeInfo) -> bool {
    let path = wt.path.to_string_lossy();
    view.dirty_worktrees.iter().any(|d| d.as_str() == path.as_ref())
}

/// Remove `wt` (`CleanWorktree`) ou, se tiver alterações não commitadas, sinaliza divergência.
/// Cada worktree é tratada no máximo uma vez.
fn clean_or_flag(
    actions: &mut Vec<Action>,
    handled: &mut HashSet<PathBuf>,
    view: &GitView,
    wt: &WorktreeInfo,
    spec_id: &str,
    task_id: &str,
) {
    if !handled.insert(wt.path.clone()) {
        return;
    }
    if is_dirty(view, wt) {
        actions.push(Action::FlagDivergence {
            spec_id: spec_id.to_string(),
            task_id: task_id.to_string(),
            reason: DIRTY_REASON.to_string(),
        });
    } else {
        actions.push(Action::CleanWorktree {
            path: wt.path.to_string_lossy().into_owned(),
        });
    }
}

/// Deriva `(spec, task)` do nome `sdd/<spec>/<task>`; vazio quando o nome não segue o padrão.
fn owner_of(branch: &str) -> (String, String) {
    branch
        .strip_prefix(TASK_BRANCH_PREFIX)
        .and_then(|rest| rest.split_once('/'))
        .map(|(s, t)| (s.to_string(), t.to_string()))
        .unwrap_or_default()
}

/// Coleta a visão Git do repositório: branches, worktrees, últimos `n` assuntos de `base_branch`
/// e as worktrees secundárias com alterações não commitadas.
pub fn collect_view(git: &Git, base_branch: &str, n: usize) -> Result<GitView, GitError> {
    if base_branch.is_empty() || base_branch.starts_with('-') {
        return Err(GitError::InvalidArg {
            name: "base_branch",
            value: base_branch.to_string(),
        });
    }
    let worktrees = git.worktree_list()?;
    let mut dirty_worktrees = Vec::new();
    for wt in worktrees.iter().skip(1) {
        // Worktree listada mas com diretório apagado (prunable): não há o que checar.
        if !wt.path.is_dir() {
            continue;
        }
        if !Git::new(wt.path.clone()).is_clean()? {
            dirty_worktrees.push(wt.path.to_string_lossy().into_owned());
        }
    }
    Ok(GitView {
        branches: git.branches()?,
        worktrees,
        base_subjects: git.log_subjects(base_branch, n)?,
        dirty_worktrees,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdd::{Settings, Spec, SpecMeta, SpecStatus, Task, TaskMeta, Verdict};
    use std::fs;
    use std::path::Path;
    use std::process::{self, Command};

    fn task(id: &str, status: TaskStatus, branch: &str) -> Task {
        Task {
            frontmatter: TaskMeta {
                id: id.to_string(),
                spec_id: "s1".to_string(),
                title: "t".to_string(),
                status,
                depends_on: vec![],
                branch: branch.to_string(),
                attempts: 0,
                max_attempts: 3,
                last_verdict: Verdict::None,
                allowed_scope: vec![],
                covers: vec![],
                reviewed_sha: None,
                rejected_sha: None,
                tested_sha: None,
                tested_signature: None,
                tested_at: None,
                test_evidence: None,
                test_run: None,
                next_action: None,
                diff_lines: vec![],
                kind: None,
            },
            body: String::new(),
        }
    }

    fn spec_named(id: &str) -> Spec {
        Spec {
            frontmatter: SpecMeta {
                id: id.to_string(),
                title: "spec".to_string(),
                status: SpecStatus::InProgress,
                base_branch: "main".to_string(),
                created_at: "2026-01-01".to_string(),
                test_command: None,
                run_command: None,
                run_url: None,
                checks: None,
                formatters: None,
                failure_artifacts: None,
                screenshots: None,
                references: None,
                environment: None,
            },
            body: String::new(),
        }
    }

    fn sdd_with(tasks: Vec<Task>) -> Sdd {
        sdd_with_base("main", tasks)
    }

    fn sdd_with_base(base: &str, tasks: Vec<Task>) -> Sdd {
        Sdd {
            settings: Settings {
                base_branch: base.to_string(),
                merge_strategy: "squash".to_string(),
                test_command: "cargo test".to_string(),
                allowed_commands: vec![],
                denied_commands: vec![],
                checks: Vec::new(),
                formatters: Vec::new(),
                failure_artifacts: Vec::new(),
                screenshots: Vec::new(),
                environment: Default::default(),
                environment_hash: None,
                approved_environment_hashes: Vec::new(),
                max_attempts: crate::sdd::DEFAULT_LIMIT,
                max_test_cycles: crate::sdd::DEFAULT_LIMIT,
                yolo: false,
                review_diff_budget: None,
                skills: true,
                ask_architect_limit: None,
                architect_internet: false,
            },
            specs: vec![(spec_named("s1"), tasks)],
        }
    }

    /// `worktrees` são pares (caminho, branch); o primeiro é a principal.
    fn view(branches: &[&str], worktrees: &[(&str, Option<&str>)], subjects: &[&str]) -> GitView {
        GitView {
            branches: branches.iter().map(|b| b.to_string()).collect(),
            worktrees: worktrees
                .iter()
                .map(|(p, b)| WorktreeInfo {
                    path: PathBuf::from(p),
                    branch: b.map(str::to_string),
                    head: "abc".to_string(),
                })
                .collect(),
            base_subjects: subjects.iter().map(|s| s.to_string()).collect(),
            dirty_worktrees: vec![],
        }
    }

    const ROOT: (&str, Option<&str>) = ("/repo", Some("main"));

    #[test]
    fn integrating_with_commit_marks_done_cleans_worktree_and_branch() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "sdd/s1/T1")]);
        let v = view(
            &["main", "sdd/s1/T1"],
            &[ROOT, ("/wt/T1", Some("sdd/s1/T1"))],
            &["feat(s1): T1 - x", "chore: inicial"],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![
                Action::MarkDone {
                    spec_id: "s1".into(),
                    task_id: "T1".into()
                },
                Action::CleanWorktree {
                    path: "/wt/T1".into()
                },
                Action::DeleteBranch {
                    branch: "sdd/s1/T1".into()
                },
            ]
        );
    }

    #[test]
    fn integrating_with_commit_and_no_leftovers_only_marks_done() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "sdd/s1/T1")]);
        let v = view(&["main"], &[ROOT], &["feat(s1): T1 - x"]);
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::MarkDone {
                spec_id: "s1".into(),
                task_id: "T1".into()
            }]
        );
    }

    #[test]
    fn commit_match_requires_exact_task_id() {
        // "T1" não pode casar com o commit de "T10".
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "")]);
        let v = view(&["main"], &[ROOT], &["feat(s1): T10 - x"]);
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::ReopenIntegrating {
                spec_id: "s1".into(),
                task_id: "T1".into()
            }]
        );
    }

    #[test]
    fn revert_of_task_commit_is_not_a_squash_commit() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "sdd/s1/T1")]);
        let v = view(
            &["main", "sdd/s1/T1"],
            &[ROOT, ("/wt/T1", Some("sdd/s1/T1"))],
            &["Revert \"feat(s1): T1 - x\""],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::ReopenIntegrating {
                spec_id: "s1".into(),
                task_id: "T1".into()
            }]
        );
    }

    #[test]
    fn empty_base_branch_fails_closed() {
        // Sem base, a guarda de DeleteBranch não tem referência: nada destrutivo é emitido.
        let sdd = sdd_with_base(
            "",
            vec![
                task("T1", TaskStatus::Integrating, "main"),
                task("T2", TaskStatus::Done, "sdd/s1/T2"),
            ],
        );
        let v = view(
            &["dev", "main", "sdd/s1/T2", "sdd/x"],
            &[
                ("/repo", Some("dev")),
                ("/wt/T2", Some("sdd/s1/T2")),
                ("/wt/x", Some("sdd/x")),
            ],
            &["feat(s1): T1 - x", "feat(s1): T2 - y"],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![
                Action::FlagDivergence {
                    spec_id: "s1".into(),
                    task_id: "T1".into(),
                    reason: "base_branch vazio".into()
                },
                Action::FlagDivergence {
                    spec_id: "s1".into(),
                    task_id: "T2".into(),
                    reason: "base_branch vazio".into()
                },
            ]
        );
    }

    #[test]
    fn dirty_worktree_is_flagged_instead_of_cleaned() {
        // T1 integrada (DONE) com worktree suja, e órfã `sdd/x` suja.
        let sdd = sdd_with(vec![task("T1", TaskStatus::Done, "sdd/s1/T1")]);
        let mut v = view(
            &["main", "sdd/s1/T1", "sdd/x"],
            &[
                ROOT,
                ("/wt/T1", Some("sdd/s1/T1")),
                ("/wt/x", Some("sdd/x")),
            ],
            &["feat(s1): T1 - x"],
        );
        v.dirty_worktrees = vec!["/wt/T1".into(), "/wt/x".into()];
        assert_eq!(
            reconcile(&sdd, &v),
            vec![
                Action::FlagDivergence {
                    spec_id: "s1".into(),
                    task_id: "T1".into(),
                    reason: "worktree com alterações não commitadas".into()
                },
                Action::FlagDivergence {
                    spec_id: "".into(),
                    task_id: "".into(),
                    reason: "worktree com alterações não commitadas".into()
                },
            ]
        );
    }

    #[test]
    fn dirty_worktree_of_integrating_task_keeps_branch() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "sdd/s1/T1")]);
        let mut v = view(
            &["main", "sdd/s1/T1"],
            &[ROOT, ("/wt/T1", Some("sdd/s1/T1"))],
            &["feat(s1): T1 - x"],
        );
        v.dirty_worktrees = vec!["/wt/T1".into()];
        assert_eq!(
            reconcile(&sdd, &v),
            vec![
                Action::MarkDone {
                    spec_id: "s1".into(),
                    task_id: "T1".into()
                },
                Action::FlagDivergence {
                    spec_id: "s1".into(),
                    task_id: "T1".into(),
                    reason: "worktree com alterações não commitadas".into()
                },
            ]
        );
    }

    #[test]
    fn integrated_task_on_base_branch_never_deletes_base() {
        // Branch da tarefa é a base (`main`), que está checada em outra worktree.
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "main")]);
        let v = view(
            &["dev", "main"],
            &[("/repo", Some("dev"))],
            &["feat(s1): T1 - x"],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::MarkDone {
                spec_id: "s1".into(),
                task_id: "T1".into()
            }]
        );
    }

    #[test]
    fn active_task_without_branch_name_does_not_orphan_derived_worktree() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::InProgress, "")]);
        let v = view(&["main", "sdd/s1/T1"], &[ROOT, ("/wt/T1", Some("sdd/s1/T1"))], &[]);
        assert!(reconcile(&sdd, &v).is_empty());
    }

    #[test]
    fn same_task_id_in_two_specs_do_not_cross() {
        let (spec_a, tasks_a) = (
            spec_named("s1"),
            vec![task("T01", TaskStatus::Integrating, "sdd/s1/T01")],
        );
        let (spec_b, tasks_b) = (
            spec_named("s2"),
            vec![task("T01", TaskStatus::Integrating, "sdd/s2/T01")],
        );
        let sdd = Sdd {
            settings: sdd_with(vec![]).settings,
            specs: vec![(spec_a, tasks_a), (spec_b, tasks_b)],
        };
        let v = view(
            &["main", "sdd/s1/T01", "sdd/s2/T01"],
            &[ROOT, ("/wt/s1", Some("sdd/s1/T01")), ("/wt/s2", Some("sdd/s2/T01"))],
            &["feat(s1): T01 - x"],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![
                Action::MarkDone {
                    spec_id: "s1".into(),
                    task_id: "T01".into()
                },
                Action::CleanWorktree {
                    path: "/wt/s1".into()
                },
                Action::DeleteBranch {
                    branch: "sdd/s1/T01".into()
                },
                Action::ReopenIntegrating {
                    spec_id: "s2".into(),
                    task_id: "T01".into()
                },
            ]
        );
    }

    #[test]
    fn integrating_without_commit_reopens() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "sdd/s1/T1")]);
        let v = view(&["main", "sdd/s1/T1"], &[ROOT], &["chore: inicial"]);
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::ReopenIntegrating {
                spec_id: "s1".into(),
                task_id: "T1".into()
            }]
        );
    }

    #[test]
    fn done_without_commit_is_flagged() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Done, "sdd/s1/T1")]);
        let v = view(&["main"], &[ROOT], &["chore: inicial"]);
        let actions = reconcile(&sdd, &v);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            Action::FlagDivergence {
                spec_id,
                task_id,
                reason,
            } => {
                assert_eq!((spec_id.as_str(), task_id.as_str()), ("s1", "T1"));
                assert!(reason.contains("DONE sem commit"), "reason: {reason}");
            }
            other => panic!("esperado FlagDivergence, veio {other:?}"),
        }
    }

    #[test]
    fn active_statuses_with_existing_branch_resume() {
        for status in [
            TaskStatus::InProgress,
            TaskStatus::InReview,
            TaskStatus::ChangesRequested,
        ] {
            let sdd = sdd_with(vec![task("T1", status, "sdd/s1/T1")]);
            let v = view(&["main", "sdd/s1/T1"], &[ROOT], &[]);
            assert_eq!(
                reconcile(&sdd, &v),
                vec![Action::ResumeInProgress {
                    spec_id: "s1".into(),
                    task_id: "T1".into(),
                    branch: "sdd/s1/T1".into()
                }],
                "status {status:?}"
            );
        }
    }

    #[test]
    fn active_status_with_missing_branch_is_flagged() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::InProgress, "sdd/s1/T1")]);
        let v = view(&["main"], &[ROOT], &[]);
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::FlagDivergence {
                spec_id: "s1".into(),
                task_id: "T1".into(),
                reason: "branch ausente: sdd/s1/T1".into()
            }]
        );
    }

    #[test]
    fn active_status_without_branch_name_is_ignored() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::InProgress, "")]);
        let v = view(&["main"], &[ROOT], &[]);
        assert!(reconcile(&sdd, &v).is_empty());
    }

    #[test]
    fn orphan_worktrees_with_sdd_prefix_are_cleaned_only() {
        let sdd = sdd_with(vec![]);
        let v = view(
            &["main", "worktree-x", "feature/y", "sdd/old"],
            &[
                ROOT,
                ("/wt/x", Some("worktree-x")),
                ("/wt/y", Some("feature/y")),
                ("/wt/old", Some("sdd/old")),
                ("/wt/detached", None),
            ],
            &[],
        );
        // `worktree-*` pode pertencer a um agente em execução: não é órfã.
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::CleanWorktree {
                path: "/wt/old".into()
            }]
        );
    }

    #[test]
    fn branch_owned_by_active_task_is_not_orphan() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::InProgress, "sdd/s1/T1")]);
        let v = view(
            &["main", "sdd/s1/T1"],
            &[ROOT, ("/wt/T1", Some("sdd/s1/T1"))],
            &[],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::ResumeInProgress {
                spec_id: "s1".into(),
                task_id: "T1".into(),
                branch: "sdd/s1/T1".into()
            }]
        );
    }

    #[test]
    fn leftover_worktree_of_done_task_is_orphan() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Done, "sdd/s1/T1")]);
        let v = view(
            &["main", "sdd/s1/T1"],
            &[ROOT, ("/wt/T1", Some("sdd/s1/T1"))],
            &["feat(s1): T1 - x"],
        );
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::CleanWorktree {
                path: "/wt/T1".into()
            }]
        );
    }

    #[test]
    fn main_worktree_is_never_cleaned() {
        // A principal está em uma branch com prefixo órfão: ainda assim não pode ser removida.
        let sdd = sdd_with(vec![]);
        let v = view(&["sdd/principal"], &[("/repo", Some("sdd/principal"))], &[]);
        assert!(reconcile(&sdd, &v).is_empty());
    }

    #[test]
    fn integrated_task_in_main_worktree_does_not_clean_root() {
        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "main")]);
        let v = view(&["main"], &[("/repo", Some("main"))], &["feat(s1): T1 - x"]);
        assert_eq!(
            reconcile(&sdd, &v),
            vec![Action::MarkDone {
                spec_id: "s1".into(),
                task_id: "T1".into()
            }]
        );
    }

    /// Aplica as ações sobre o estado SDD e a visão Git, simulando a efetivação.
    fn apply(sdd: &mut Sdd, view: &mut GitView, actions: &[Action]) {
        for action in actions {
            match action {
                Action::MarkDone { task_id, .. } => set_status(sdd, task_id, TaskStatus::Done),
                Action::ReopenIntegrating { task_id, .. } => {
                    set_status(sdd, task_id, TaskStatus::Approved)
                }
                Action::CleanWorktree { path } => {
                    view.worktrees.retain(|w| w.path.to_string_lossy() != path.as_str());
                }
                Action::DeleteBranch { branch } => view.branches.retain(|b| b != branch),
                Action::FlagDivergence { .. } | Action::ResumeInProgress { .. } => {}
            }
        }
    }

    fn set_status(sdd: &mut Sdd, task_id: &str, status: TaskStatus) {
        for (_, tasks) in &mut sdd.specs {
            for t in tasks.iter_mut().filter(|t| t.frontmatter.id == task_id) {
                t.frontmatter.status = status;
            }
        }
    }

    #[test]
    fn reconcile_is_idempotent_after_applying_actions() {
        let mut sdd = sdd_with(vec![
            task("T1", TaskStatus::Integrating, "sdd/s1/T1"),
            task("T2", TaskStatus::Integrating, "sdd/s1/T2"),
            task("T3", TaskStatus::InProgress, "sdd/s1/T3"),
        ]);
        let mut v = view(
            &["main", "sdd/s1/T1", "sdd/s1/T2", "sdd/s1/T3", "sdd/orfa"],
            &[
                ROOT,
                ("/wt/T1", Some("sdd/s1/T1")),
                ("/wt/T2", Some("sdd/s1/T2")),
                ("/wt/T3", Some("sdd/s1/T3")),
                ("/wt/orfa", Some("sdd/orfa")),
            ],
            &["feat(s1): T1 - x"],
        );

        let first = reconcile(&sdd, &v);
        assert!(first.contains(&Action::MarkDone {
            spec_id: "s1".into(),
            task_id: "T1".into()
        }));
        assert!(first.contains(&Action::ReopenIntegrating {
            spec_id: "s1".into(),
            task_id: "T2".into()
        }));
        assert!(first.contains(&Action::CleanWorktree {
            path: "/wt/orfa".into()
        }));

        apply(&mut sdd, &mut v, &first);
        let second = reconcile(&sdd, &v);
        // Efetivadas: MarkDone/limpeza de T1 e órfã não se repetem. T2 virou APPROVED
        // (sem ação); T3 só gera ResumeInProgress, que é informativa.
        assert_eq!(
            second,
            vec![Action::ResumeInProgress {
                spec_id: "s1".into(),
                task_id: "T3".into(),
                branch: "sdd/s1/T3".into()
            }]
        );
        apply(&mut sdd, &mut v, &second);
        assert_eq!(reconcile(&sdd, &v), second, "informativas são reemitidas");
    }

    /// Cria um repositório temporário com commit `feat(s1): T1 - x` e uma worktree extra.
    fn git_fixture(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("nants-recon-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).expect("mkdir repo");
        git_in(&repo, &["init", "-b", "main"]);
        git_in(&repo, &["config", "user.name", "Nants Test"]);
        git_in(&repo, &["config", "user.email", "test@example.com"]);
        git_in(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("README.md"), "x\n").expect("write");
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-m", "feat(s1): T1 - x"]);
        (base, repo)
    }

    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} falhou: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn collect_view_reads_real_repository() {
        let (base, repo) = git_fixture("collect");
        let wt = base.join("wt-orfa");
        git_in(
            &repo,
            &["worktree", "add", "-B", "sdd/orfa", wt.to_str().expect("utf8"), "main"],
        );
        let git = Git::new(&repo);

        let v = collect_view(&git, "main", 10).expect("collect");
        assert!(v.branches.contains(&"main".to_string()));
        assert!(v.branches.contains(&"sdd/orfa".to_string()));
        assert_eq!(v.worktrees.len(), 2);
        assert_eq!(v.worktrees[0].branch.as_deref(), Some("main"));
        assert_eq!(v.base_subjects, vec!["feat(s1): T1 - x"]);

        let sdd = sdd_with(vec![task("T1", TaskStatus::Integrating, "sdd/s1/T1")]);
        let actions = reconcile(&sdd, &v);
        assert_eq!(
            actions,
            vec![
                Action::MarkDone {
                    spec_id: "s1".into(),
                    task_id: "T1".into()
                },
                Action::CleanWorktree {
                    path: wt.to_string_lossy().into_owned()
                },
            ]
        );
    }

    #[test]
    fn collect_view_detects_dirty_worktree() {
        let (base, repo) = git_fixture("collect-dirty");
        let limpa = base.join("wt-limpa");
        let suja = base.join("wt-suja");
        git_in(
            &repo,
            &["worktree", "add", "-B", "sdd/limpa", limpa.to_str().expect("utf8"), "main"],
        );
        git_in(
            &repo,
            &["worktree", "add", "-B", "sdd/suja", suja.to_str().expect("utf8"), "main"],
        );
        fs::write(suja.join("novo.txt"), "não commitado\n").expect("write untracked");
        let git = Git::new(&repo);

        let v = collect_view(&git, "main", 10).expect("collect");
        let suja_s = suja.to_string_lossy().into_owned();
        assert_eq!(v.dirty_worktrees, vec![suja_s.clone()]);
        assert!(!v.dirty_worktrees.contains(&limpa.to_string_lossy().into_owned()));
        assert!(!v.dirty_worktrees.contains(&repo.to_string_lossy().into_owned()));

        let sdd = sdd_with(vec![]);
        // A limpa `sdd/limpa` é órfã e sai; a suja `sdd/suja` só é sinalizada.
        assert_eq!(
            reconcile(&sdd, &v),
            vec![
                Action::CleanWorktree {
                    path: limpa.to_string_lossy().into_owned()
                },
                Action::FlagDivergence {
                    spec_id: "".into(),
                    task_id: "".into(),
                    reason: "worktree com alterações não commitadas".into()
                },
            ]
        );
    }

    #[test]
    fn collect_view_tolerates_worktree_directory_removed() {
        let (base, repo) = git_fixture("collect-prunable");
        let wt = base.join("wt-apagada");
        git_in(
            &repo,
            &["worktree", "add", "-B", "sdd/s1/T1", wt.to_str().expect("utf8"), "main"],
        );
        fs::remove_dir_all(&wt).expect("remove worktree dir");
        let git = Git::new(&repo);

        let v = collect_view(&git, "main", 10).expect("collect deve tolerar diretório ausente");
        assert!(v.dirty_worktrees.is_empty());
        let wt_s = wt.to_string_lossy().into_owned();
        assert!(v.worktrees.iter().any(|w| w.path.to_string_lossy() == wt_s));
        assert!(v.branches.contains(&"sdd/s1/T1".to_string()));
        assert_eq!(v.base_subjects, vec!["feat(s1): T1 - x"]);
    }

    #[test]
    fn collect_view_rejects_option_like_base() {
        let (_base, repo) = git_fixture("collect-arg");
        let git = Git::new(&repo);
        assert!(matches!(
            collect_view(&git, "--output=x", 5),
            Err(GitError::InvalidArg {
                name: "base_branch",
                ..
            })
        ));
        assert!(matches!(
            collect_view(&git, "", 5),
            Err(GitError::InvalidArg {
                name: "base_branch",
                ..
            })
        ));
    }
}
