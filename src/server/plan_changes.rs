//! "O que mudou no plano" entre versões (spec 086 T09).
//!
//! Antes do turno do chat, [`snapshots`] guarda (em memória) o retrato estruturado de cada spec
//! em `DRAFT`/`AWAITING_APPROVAL` (texto já redigido). Depois de um turno que alterou artefatos,
//! [`record_changes`] compara com o plano atual e grava `.sdd/specs/<spec>/plan-changes.json`
//! (`{signature, lines}`; escrita atômica, sem seguir symlink, local via `.git/info/exclude`).
//! Spec sem retrato anterior (primeiro plano) não grava nada; mudança só de formatação grava a
//! lista vazia. O estado expõe `plan_changes` só quando a assinatura gravada (a mesma da T08) bate
//! com o conteúdo atual.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::events::SecretSet;
use super::plan_review::{plan_signature, task_names, write_local_file};
use super::projects::{read_spec, read_task, real_children, scrub_text};
use super::report::exclude_guaranteed;
use super::safe_fs;
use super::spec_result::validate_spec_id;
use crate::sdd::plan_diff::{diff_lines, ChangeLine, PlanSnapshot, MAX_CHANGE_LINES};
use crate::sdd::{Spec, SpecStatus};

/// Nome do arquivo do resumo, dentro de `.sdd/specs/<id>/`.
pub const PLAN_CHANGES_FILE: &str = "plan-changes.json";
/// Linhas do `.git/info/exclude`: o resumo é local (não suja a base nem entra no plano).
pub const PLAN_CHANGES_EXCLUDE_LINE: &str = ".sdd/specs/*/plan-changes.json";
pub const PLAN_CHANGES_TMP_EXCLUDE_LINE: &str = ".sdd/specs/*/.plan-changes.json.*.tmp";

const SPECS_REL: &str = ".sdd/specs";
const MAX_RECORD_BYTES: u64 = 32 * 1024;
/// Teto de cada linha exposta (bytes, depois da redação).
const MAX_LINE_BYTES: usize = 200;

/// Conteúdo de `plan-changes.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanChangesRecord {
    /// Assinatura do plano (spec 086 T08) da versão descrita.
    pub signature: String,
    pub lines: Vec<PlanChangeLine>,
}

/// Uma linha do resumo, como gravada e como vai ao estado: com `code` e `params` desde a spec 131
/// T06 (a web traduz), ou o texto em pt-BR dos arquivos gravados antes, que continua sendo lido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PlanChangeLine {
    Coded(ChangeLine),
    Text(String),
}

impl PlanChangeLine {
    /// Texto da linha no idioma (a linha antiga sai como foi gravada).
    pub fn render(&self, lang: crate::i18n::Lang) -> String {
        match self {
            Self::Coded(line) => line.render(lang),
            Self::Text(text) => text.clone(),
        }
    }
}

/// Código de linha aceito na leitura (o formato dos códigos de `plan_diff`).
fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 40 && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
}

/// Retratos do plano por spec, tirados antes do turno: assinatura do plano (T08) e retrato.
pub type Snapshots = BTreeMap<String, (String, PlanSnapshot)>;

fn in_plan(spec: &Spec) -> bool {
    matches!(spec.frontmatter.status, SpecStatus::Draft | SpecStatus::AwaitingApproval)
}

/// Specs em plano (pastas reais, não ocultas, de id válido), com a spec lida.
fn plan_specs(root: &Path) -> Vec<(String, Spec)> {
    if safe_fs::check_chain(root, SPECS_REL).is_err() {
        return Vec::new();
    }
    real_children(&root.join(SPECS_REL), true)
        .into_iter()
        .filter(|name| !name.starts_with('.') && validate_spec_id(name).is_ok())
        .filter_map(|name| {
            let spec = read_spec(root, &format!("{SPECS_REL}/{name}/spec.md")).ok()?;
            in_plan(&spec).then_some((name, spec))
        })
        .collect()
}

/// Ids das specs em plano (`DRAFT`/`AWAITING_APPROVAL`), na ordem das pastas (spec 136 T05: a
/// sessão do plano das menções).
pub(super) fn plan_spec_ids(root: &Path) -> Vec<String> {
    plan_specs(root).into_iter().map(|(id, _)| id).collect()
}

/// Retrato redigido de uma spec: cada texto é redigido antes de qualquer corte.
fn snapshot_of(root: &Path, spec_id: &str, spec: &Spec, secrets: &SecretSet) -> PlanSnapshot {
    let tasks: Vec<_> = task_names(root, spec_id)
        .iter()
        .filter_map(|name| read_task(root, &format!("{SPECS_REL}/{spec_id}/tasks/{name}")).ok())
        .collect();
    let mut snap = PlanSnapshot::from_parts(spec, &tasks);
    let scrub = |s: &mut String| *s = scrub_text(secrets, s);
    scrub(&mut snap.title);
    if let Some(cmd) = snap.test_command.as_mut() {
        scrub(cmd);
    }
    snap.global_criteria.iter_mut().for_each(scrub);
    for task in &mut snap.tasks {
        scrub(&mut task.id);
        scrub(&mut task.title);
        task.depends_on.iter_mut().for_each(scrub);
        task.allowed_scope.iter_mut().for_each(scrub);
        task.criteria.iter_mut().for_each(scrub);
        task.covers.iter_mut().for_each(scrub);
    }
    snap
}

/// Retratos de todas as specs em plano (`DRAFT`/`AWAITING_APPROVAL`), com a assinatura de cada uma.
pub fn snapshots(root: &Path, secrets: &SecretSet) -> Snapshots {
    plan_specs(root)
        .into_iter()
        .filter_map(|(id, spec)| {
            let signature = plan_signature(root, &id)?;
            let snap = snapshot_of(root, &id, &spec, secrets);
            Some((id, (signature, snap)))
        })
        .collect()
}

/// Depois de um turno que alterou artefatos: compara cada spec em plano que tinha retrato e cuja
/// assinatura mudou, e grava o resumo. Spec que o turno não alterou (mesma assinatura) é pulada: o
/// resumo da versão atual continua valendo. Sem exclude local garantido, nada é gravado. Devolve as
/// specs gravadas.
pub fn record_changes(root: &Path, before: &Snapshots, secrets: &SecretSet) -> Vec<String> {
    if before.is_empty() || !exclude_guaranteed(root) {
        return Vec::new();
    }
    plan_specs(root)
        .into_iter()
        .filter_map(|(id, spec)| {
            let (old_signature, old) = before.get(&id)?;
            let signature = plan_signature(root, &id)?;
            if &signature == old_signature {
                return None;
            }
            let new = snapshot_of(root, &id, &spec, secrets);
            let record = PlanChangesRecord { signature, lines: diff_lines(old, &new).into_iter().map(PlanChangeLine::Coded).collect() };
            let body = serde_json::to_string_pretty(&record).ok()?;
            write_local_file(root, &record_rel(&id), PLAN_CHANGES_FILE, &body)?;
            Some(id)
        })
        .collect()
}

/// Specs em plano alteradas desde `before` (id → assinatura do plano antes do turno): assinatura
/// diferente ou spec sem retrato anterior.
pub fn changed_specs(root: &Path, before: &BTreeMap<String, String>) -> Vec<(String, Spec)> {
    plan_specs(root)
        .into_iter()
        .filter(|(id, _)| plan_signature(root, id).is_some_and(|sig| before.get(id) != Some(&sig)))
        .collect()
}

fn record_rel(spec_id: &str) -> String {
    format!("{SPECS_REL}/{spec_id}/{PLAN_CHANGES_FILE}")
}

/// Resumo gravado (arquivo regular, sem symlink). `None` se ausente, inseguro ou inválido.
fn read_record(root: &Path, spec_id: &str) -> Option<PlanChangesRecord> {
    let raw = safe_fs::read_regular(root, &record_rel(spec_id), MAX_RECORD_BYTES).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Redige, junta em uma linha e corta (nessa ordem).
fn clean_line(secrets: &SecretSet, text: &str) -> String {
    let scrubbed = scrub_text(secrets, text);
    let line = scrubbed.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.len() <= MAX_LINE_BYTES {
        return line;
    }
    let cut = crate::agents::architect::truncate_bytes(&line, MAX_LINE_BYTES - '…'.len_utf8());
    format!("{}…", cut.trim_end())
}

/// Linha lida de novo: texto e parâmetros redigidos e cortados; código fora do formato descarta a
/// linha, e o parâmetro que não é texto nem número sai.
fn clean_entry(secrets: &SecretSet, line: &PlanChangeLine) -> Option<PlanChangeLine> {
    match line {
        PlanChangeLine::Text(text) => {
            let line = clean_line(secrets, text);
            (!line.is_empty()).then_some(PlanChangeLine::Text(line))
        }
        PlanChangeLine::Coded(coded) => {
            if !valid_code(&coded.code) {
                return None;
            }
            let params = coded
                .params
                .iter()
                .filter_map(|(k, v)| match v {
                    serde_json::Value::String(s) => Some((clean_line(secrets, k), serde_json::Value::from(clean_line(secrets, s)))),
                    serde_json::Value::Number(_) => Some((clean_line(secrets, k), v.clone())),
                    _ => None,
                })
                .collect();
            Some(PlanChangeLine::Coded(ChangeLine { code: coded.code.clone(), params }))
        }
    }
}

/// `plan_changes` de uma spec: as linhas gravadas, só se a assinatura bate com o conteúdo atual e
/// há ao menos uma linha (redigidas de novo na leitura, no máximo [`MAX_CHANGE_LINES`]).
pub fn view(root: &Path, spec_id: &str, secrets: &SecretSet) -> Option<Vec<PlanChangeLine>> {
    let record = read_record(root, spec_id)?;
    if plan_signature(root, spec_id)? != record.signature {
        return None;
    }
    let lines: Vec<PlanChangeLine> = record.lines.iter().filter_map(|l| clean_entry(secrets, l)).take(MAX_CHANGE_LINES).collect();
    (!lines.is_empty()).then_some(lines)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::agents::prompts::{EXAMPLE_SPEC_MD, EXAMPLE_TASK_MD};

    const SPEC: &str = "001-exemplo";
    const SECRET: &str = "sk-SEGREDO-MUDANCA-0123456789";

    fn tmp(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("nants-plan-changes-{}-{n}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".git/info")).expect("git");
        fs::create_dir_all(dir.join(format!(".sdd/specs/{SPEC}/tasks"))).expect("mkdir");
        dir
    }

    fn spec_dir(root: &Path) -> PathBuf {
        root.join(format!(".sdd/specs/{SPEC}"))
    }

    fn write_plan(root: &Path, spec: &str, tasks: &[(&str, &str)]) {
        fs::write(spec_dir(root).join("spec.md"), spec).expect("spec");
        for (name, body) in tasks {
            fs::write(spec_dir(root).join(format!("tasks/{name}")), body).expect("task");
        }
    }

    fn secrets() -> SecretSet {
        SecretSet::new([SECRET.to_string()])
    }

    fn record(root: &Path) -> Option<PlanChangesRecord> {
        read_record(root, SPEC)
    }

    /// Linhas em pt-BR.
    fn texts(lines: &[PlanChangeLine]) -> Vec<String> {
        lines.iter().map(|l| l.render(crate::i18n::Lang::PtBr)).collect()
    }

    /// Spec 131 T06: a linha nova é gravada com `code` e `params` (sem idioma) e sai assim no estado.
    #[test]
    fn lines_are_written_with_code_and_params() {
        let root = tmp("coded");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        fs::remove_file(spec_dir(&root).join("tasks/T01.md")).expect("rm");
        record_changes(&root, &before, &secrets());
        let raw: serde_json::Value = serde_json::from_str(&fs::read_to_string(spec_dir(&root).join(PLAN_CHANGES_FILE)).expect("raw")).expect("json");
        assert_eq!(raw["lines"], serde_json::json!([{"code": "task_removed", "params": {"id": "T01"}}]));
        let lines = view(&root, SPEC, &secrets()).expect("linhas");
        assert_eq!(serde_json::to_value(&lines).expect("estado"), raw["lines"]);
        assert_eq!(lines[0].render(crate::i18n::Lang::En), "T01 removed");
    }

    /// Spec 131 T06: o arquivo gravado antes, com as linhas em texto (o do Test 9), continua lido,
    /// e uma linha com código fora do formato é descartada.
    #[test]
    fn old_text_record_is_still_read() {
        let root = tmp("old-text");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let signature = plan_signature(&root, SPEC).expect("assinatura");
        let body = serde_json::json!({
            "lines": ["T07: dependências alteradas", "T06 nova: Provide executable launcher with CLI parity", {"code": "Inválido!", "params": {}}],
            "signature": signature,
        });
        fs::write(spec_dir(&root).join(PLAN_CHANGES_FILE), body.to_string()).expect("grava");
        let lines = view(&root, SPEC, &secrets()).expect("linhas");
        assert_eq!(texts(&lines), vec!["T07: dependências alteradas", "T06 nova: Provide executable launcher with CLI parity"]);
        assert_eq!(serde_json::to_value(&lines).expect("estado")[0], "T07: dependências alteradas");
    }

    #[test]
    fn changed_specs_are_new_or_with_other_signature() {
        // Spec 092 T01: só as specs que o turno alterou entram na conferência dos avisos.
        let root = tmp("changed");
        let sigs = |snaps: &Snapshots| snaps.iter().map(|(id, (sig, _))| (id.clone(), sig.clone())).collect::<BTreeMap<_, _>>();
        let ids = |list: Vec<(String, Spec)>| list.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        assert_eq!(ids(changed_specs(&root, &BTreeMap::new())), vec![SPEC], "spec nova");
        let before = sigs(&snapshots(&root, &secrets()));
        assert!(changed_specs(&root, &before).is_empty(), "sem alteração");
        write_plan(&root, &format!("{EXAMPLE_SPEC_MD}\n"), &[]);
        assert_eq!(ids(changed_specs(&root, &before)), vec![SPEC]);
    }

    #[test]
    fn first_plan_writes_nothing() {
        let root = tmp("first");
        let before = snapshots(&root, &secrets());
        assert!(before.is_empty(), "sem plano não há retrato");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        assert!(record_changes(&root, &before, &secrets()).is_empty());
        assert!(!spec_dir(&root).join(PLAN_CHANGES_FILE).exists());
    }

    #[test]
    fn formatting_only_writes_empty_list_and_view_omits_it() {
        let root = tmp("format");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        write_plan(&root, &format!("{EXAMPLE_SPEC_MD}\n\n"), &[("T01.md", &EXAMPLE_TASK_MD.replace("- [ ] Critério verificável.", "1.  [ ]   Critério   verificável.\n"))]);
        assert_eq!(record_changes(&root, &before, &secrets()), vec![SPEC.to_string()]);
        let rec = record(&root).expect("gravado");
        assert!(rec.lines.is_empty(), "{:?}", rec.lines);
        assert!(view(&root, SPEC, &secrets()).is_none());
        let exclude = fs::read_to_string(root.join(".git/info/exclude")).expect("exclude");
        assert!(exclude.contains(PLAN_CHANGES_EXCLUDE_LINE) && exclude.contains(PLAN_CHANGES_TMP_EXCLUDE_LINE));
    }

    #[test]
    fn new_task_line_is_redacted_and_view_matches_current_version() {
        let root = tmp("new-task");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        let t02 = EXAMPLE_TASK_MD.replacen("id: \"T01\"", "id: \"T02\"", 1);
        let t02 = replace_title(&t02, &format!("Usa {SECRET} no deploy"));
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T02.md", &t02)]);
        record_changes(&root, &before, &secrets());
        let lines = texts(&view(&root, SPEC, &secrets()).expect("linhas"));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("T02 nova: Usa "), "{lines:?}");
        assert!(!lines[0].contains(SECRET), "redigido: {lines:?}");
        let raw = fs::read_to_string(spec_dir(&root).join(PLAN_CHANGES_FILE)).expect("raw");
        assert!(!raw.contains(SECRET), "gravado já redigido");

        // Plano mudou de novo sem novo resumo: o antigo some do estado.
        write_plan(&root, &format!("{EXAMPLE_SPEC_MD}\nmais\n"), &[]);
        assert!(view(&root, SPEC, &secrets()).is_none());
    }

    #[test]
    fn removed_task_and_test_command_lines() {
        let root = tmp("removed");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        fs::remove_file(spec_dir(&root).join("tasks/T01.md")).expect("rm");
        let spec = with_test_command(EXAMPLE_SPEC_MD, "npm test -- --run");
        write_plan(&root, &spec, &[]);
        record_changes(&root, &before, &secrets());
        let lines = texts(&view(&root, SPEC, &secrets()).expect("linhas"));
        assert!(lines.contains(&"test_command alterado".to_string()), "{lines:?}");
        assert!(lines.iter().any(|l| l.ends_with(" removida")), "{lines:?}");
    }

    #[test]
    fn unchanged_spec_keeps_current_summary() {
        // F-01: turno que grava outro artefato não troca o resumo válido da versão atual por `[]`.
        let root = tmp("unchanged");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        fs::remove_file(spec_dir(&root).join("tasks/T01.md")).expect("rm");
        record_changes(&root, &before, &secrets());
        let lines = view(&root, SPEC, &secrets()).expect("resumo");
        assert_eq!(texts(&lines), vec!["T01 removida"]);

        // Novo turno com `artifacts_touched` que mexe só em outra spec.
        let before = snapshots(&root, &secrets());
        let other = root.join(".sdd/specs/002-outra");
        fs::create_dir_all(&other).expect("outra");
        fs::write(other.join("spec.md"), EXAMPLE_SPEC_MD.replace("001-exemplo", "002-outra")).expect("spec");
        assert!(record_changes(&root, &before, &secrets()).is_empty(), "spec inalterada é pulada");
        assert_eq!(view(&root, SPEC, &secrets()).expect("mantido"), lines);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_record_is_not_followed() {
        let root = tmp("symlink");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        let outside = root.join("fora.json");
        fs::write(&outside, "original").expect("fora");
        std::os::unix::fs::symlink(&outside, spec_dir(&root).join(PLAN_CHANGES_FILE)).expect("link");
        write_plan(&root, &format!("{EXAMPLE_SPEC_MD}\n"), &[]);
        assert!(record_changes(&root, &before, &secrets()).is_empty());
        assert_eq!(fs::read_to_string(&outside).expect("fora"), "original");
        assert!(view(&root, SPEC, &secrets()).is_none());
    }

    #[test]
    fn gitfile_worktree_writes_nothing() {
        let root = tmp("gitfile");
        fs::remove_dir_all(root.join(".git")).expect("rm git");
        fs::write(root.join(".git"), "gitdir: /x").expect("gitfile");
        write_plan(&root, EXAMPLE_SPEC_MD, &[("T01.md", EXAMPLE_TASK_MD)]);
        let before = snapshots(&root, &secrets());
        write_plan(&root, &with_test_command(EXAMPLE_SPEC_MD, "make check"), &[]);
        assert!(record_changes(&root, &before, &secrets()).is_empty());
        assert!(!spec_dir(&root).join(PLAN_CHANGES_FILE).exists());
    }

    /// Troca o `title:` do frontmatter.
    fn replace_title(md: &str, title: &str) -> String {
        md.lines()
            .map(|l| if l.starts_with("title:") { format!("title: \"{title}\"") } else { l.to_string() })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    /// Define (ou troca) o `test_command:` do frontmatter.
    fn with_test_command(md: &str, cmd: &str) -> String {
        let line = format!("test_command: \"{cmd}\"");
        if md.lines().any(|l| l.starts_with("test_command:")) {
            return md
                .lines()
                .map(|l| if l.starts_with("test_command:") { line.clone() } else { l.to_string() })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
        }
        md.replacen("title:", &format!("{line}\ntitle:"), 1)
    }
}
