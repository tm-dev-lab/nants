//! Leitura a pedido do projeto citado na conversa (spec 138 T08, C-13).
//!
//! - **De onde:** o snapshot do projeto (`server::snapshot`, o mesmo do Arquiteto), que já trata o
//!   texto do projeto como não confiável, redige segredos e URLs antes do corte por campo e tem teto;
//!   o relatório final mais recente (`report.md`, redigido e cortado); e o custo do projeto pelo
//!   `usage.jsonl` (só números).
//! - **Sem logs:** nenhum `run.log`, saída de agente, trilha ou diálogo entra; os códigos de parada
//!   já chegam pelos sinais estruturados.
//! - **Só o projeto citado:** a conversa chama isto para um projeto só, o que a pessoa citou.
//! - **Dado não confiável:** o texto lido vai em `untrusted`; o prompt manda tratar tudo ali como dado
//!   e nunca como instrução. O caminho do projeto na máquina fica de fora.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use super::turn::is_code;
use crate::config::{Config, Project};
use crate::server::events::SecretSet;
use crate::server::safe_fs;
use crate::server::state::AppState;

/// Teto do relatório levado ao modelo, em bytes.
pub const REPORT_MAX_BYTES: usize = 1500;
/// Teto do nome do projeto, em bytes.
const NAME_MAX_BYTES: usize = 100;
/// Pasta das specs.
const SPECS_REL: &str = ".sdd/specs";
/// Linha do snapshot com o caminho do projeto na máquina (fica de fora).
const PATH_LINE: &str = "Projeto: ";

/// O snapshot sem a linha do caminho do projeto na máquina.
pub fn without_path(snapshot: &str) -> String {
    snapshot.lines().filter(|l| !l.starts_with(PATH_LINE)).collect::<Vec<_>>().join("\n")
}

/// O relatório final mais recente: `(spec, texto)`, redigido e cortado. `None` sem relatório legível.
pub fn latest_report(root: &Path, secrets: &SecretSet) -> Option<(String, String)> {
    safe_fs::check_chain(root, SPECS_REL).ok()?;
    let mut names: Vec<String> = std::fs::read_dir(root.join(SPECS_REL))
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| is_code(n) && crate::server::spec_result::validate_spec_id(n).is_ok())
        .collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    names.into_iter().find_map(|name| {
        let rel = format!("{SPECS_REL}/{name}/report.md");
        let text = safe_fs::read_regular(root, &rel, crate::server::report::REPORT_MAX_BYTES as u64).ok()?;
        let clean = crate::server::snapshot::clean_field(secrets, &text, REPORT_MAX_BYTES);
        (!clean.is_empty()).then_some((name, clean))
    })
}

/// O custo do projeto (todas as specs e o chat): chamadas, tokens e US$ do que tem preço.
pub fn project_cost(root: &Path, pricing: &std::collections::BTreeMap<String, crate::config::Pricing>) -> Value {
    let records = crate::server::usage::load_records(root, None);
    let total = crate::server::usage::aggregate(&records, pricing).total;
    json!({
        "calls": total.calls,
        "input_tokens": total.input,
        "output_tokens": total.output,
        "cost_usd": total.cost_usd.map(|v| (v * 10_000.0).round() / 10_000.0),
        "unpriced_calls": total.unpriced_calls,
    })
}

/// A leitura do projeto citado: o id e se há execução (fatos do servidor), o custo (números) e, em
/// `untrusted`, o que veio dos arquivos (o nome, o snapshot e o relatório).
pub async fn project_view(state: &AppState, cfg: &Config, project: &Project) -> Value {
    let root = PathBuf::from(&project.path);
    let secrets: Arc<SecretSet> = state.secret_set().await;
    let running = state.runs().active(&project.id).is_some();
    let snapshot = crate::server::snapshot::project_snapshot(root.clone(), Arc::clone(&secrets), running).await;
    let snapshot = without_path(&snapshot);
    let name = crate::server::snapshot::clean_field(&secrets, &project.name, NAME_MAX_BYTES);
    let pricing = cfg.pricing.clone();
    let files = Arc::clone(&secrets);
    let job = tokio::task::spawn_blocking(move || (latest_report(&root, &files), project_cost(&root, &pricing)));
    let (report, cost) = job.await.unwrap_or((None, Value::Null));
    let report = report.map(|(spec, text)| json!({ "spec_id": spec, "text": text }));
    json!({
        "id": Some(project.id.as_str()).filter(|id| is_code(id)),
        "running": running,
        "cost": cost,
        "untrusted": {
            "name": name,
            "snapshot": snapshot,
            "report": report,
        },
    })
}
