//! Rastreabilidade critério global → tarefas (spec 086 T07). Cada tarefa declara em `covers` os
//! ids `G<n>` dos critérios globais que entrega (`G1` = 1º item de "Critérios de Aceitação
//! Globais" da spec). Funções puras: os avisos nunca bloqueiam e planos sem `covers` não geram
//! aviso nem seção de rastreabilidade.

use crate::i18n::{tr, Lang};
use serde::{Deserialize, Deserializer};

use super::Task;
use crate::agents::runner::{section_text, GLOBAL_CRITERIA};

/// Tamanho (em caracteres) do trecho do critério citado nos avisos e no relatório.
pub const CRITERION_SNIPPET_CHARS: usize = 60;
/// Máximo de ids aceitos em `covers` de uma tarefa (o resto é ignorado).
const MAX_COVERS: usize = 64;

/// Normaliza um id `G<n>` (n ≥ 1, sem espaços internos). `None` se malformado.
pub fn normalize_cover(raw: &str) -> Option<String> {
    let digits = raw.trim().strip_prefix('G')?;
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    (n >= 1).then(|| format!("G{n}"))
}

/// Ids válidos, sem repetição, na ordem em que aparecem.
pub fn validate_covers<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in items.into_iter().filter_map(normalize_cover) {
        if !out.contains(&id) && out.len() < MAX_COVERS {
            out.push(id);
        }
    }
    out
}

/// `covers` tolerante: lista (itens não texto ou malformados são ignorados) ou texto único
/// separado por vírgulas/espaços. Qualquer outra forma equivale a vazio; a tarefa segue válida.
pub fn deserialize_covers<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<String>, D::Error> {
    let value = Option::<serde_yaml::Value>::deserialize(de)?;
    Ok(match value {
        Some(serde_yaml::Value::Sequence(items)) => validate_covers(items.iter().filter_map(|v| v.as_str())),
        Some(serde_yaml::Value::String(text)) => {
            validate_covers(text.split(|c: char| c == ',' || c.is_whitespace()))
        }
        _ => Vec::new(),
    })
}

/// Itens de primeiro nível de "## Critérios de Aceitação Globais" (sem marcador nem caixa `[ ]`),
/// na ordem da spec: o índice 0 é `G1`.
pub fn global_criteria(spec_body: &str) -> Vec<String> {
    let Some(text) = section_text(spec_body, GLOBAL_CRITERIA) else {
        return Vec::new();
    };
    text.lines().filter_map(criterion_item).collect()
}

/// Texto de uma linha de item (`- `, `* `, `1. `, `1) `), sem recuo de sublista.
fn criterion_item(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    if indent > 1 {
        return None;
    }
    let t = line.trim();
    let rest = if let Some(r) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        r
    } else {
        let digits = t.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        t[digits..].strip_prefix(". ").or_else(|| t[digits..].strip_prefix(") "))?
    };
    let rest = rest.trim_start();
    let rest = ["[ ]", "[x]", "[X]"]
        .iter()
        .find_map(|b| rest.strip_prefix(b))
        .unwrap_or(rest)
        .trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Início do texto do critério, até [`CRITERION_SNIPPET_CHARS`] caracteres (com `…` se cortado).
/// Sem marcadores `**`/`__` e sem o prefixo `G<n>` + separador (a linha já cita o id).
pub fn snippet(text: &str) -> String {
    let plain = drop_emphasis_markers(text);
    let flat = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let flat = strip_criterion_id(&flat).to_string();
    if flat.chars().count() <= CRITERION_SNIPPET_CHARS {
        return flat;
    }
    let cut: String = flat.chars().take(CRITERION_SNIPPET_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// Remove os marcadores `**`/`__` (sequência de exatamente dois). Sequências maiores ficam: o
/// `***` da redação de segredos não pode virar `*`.
fn drop_emphasis_markers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' || c == '_' {
            let run = chars[i..].iter().take_while(|&&x| x == c).count();
            if run != 2 {
                out.extend(std::iter::repeat_n(c, run));
            }
            i += run;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Remove um prefixo inicial `G<n>` seguido de separador (`—`, `–`, `-`, `.`, `:`) e espaços.
/// Sem separador (ex.: "G1 texto", "G12abc"), o texto fica como está.
fn strip_criterion_id(text: &str) -> &str {
    let Some(rest) = text.strip_prefix('G') else {
        return text;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return text;
    }
    let after = rest[digits..].trim_start();
    match ['—', '–', '-', '.', ':'].iter().find_map(|sep| after.strip_prefix(*sep)) {
        Some(tail) => tail.trim_start(),
        None => text,
    }
}

/// `true` quando ao menos uma tarefa declara `covers` (planos antigos não declaram).
fn plan_declares_covers(tasks: &[Task]) -> bool {
    tasks.iter().any(|t| !t.frontmatter.covers.is_empty())
}

/// Avisos de cobertura do plano, na ordem: ids inexistentes por tarefa, depois critérios globais
/// sem tarefa. Sem nenhum `covers` no plano, nenhum aviso.
pub fn coverage_warnings(spec_body: &str, tasks: &[Task]) -> Vec<String> {
    if !plan_declares_covers(tasks) {
        return Vec::new();
    }
    let criteria = global_criteria(spec_body);
    let exists = |id: &str| index_of(id).is_some_and(|i| i < criteria.len());
    let mut out = Vec::new();
    for task in tasks {
        for id in task.frontmatter.covers.iter().filter(|id| !exists(id)) {
            out.push(format!("{}: covers cita {id}, que não existe", task.frontmatter.id));
        }
    }
    for (i, text) in criteria.iter().enumerate() {
        if tasks_covering(tasks, i).next().is_none() {
            out.push(format!("G{} ({}) não é coberto por nenhuma tarefa", i + 1, snippet(text)));
        }
    }
    out
}

/// Índice (0 = `G1`) de um id já normalizado.
fn index_of(id: &str) -> Option<usize> {
    let n: usize = id.strip_prefix('G')?.parse().ok()?;
    n.checked_sub(1)
}

/// Tarefas que declaram o critério de índice `i`, na ordem do plano.
fn tasks_covering(tasks: &[Task], i: usize) -> impl Iterator<Item = &Task> {
    tasks
        .iter()
        .filter(move |t| t.frontmatter.covers.iter().any(|id| index_of(id) == Some(i)))
}

/// Seção "## Rastreabilidade" do relatório, montada sem o modelo: uma linha por critério global,
/// "G1: <texto curto> (T01, commit abc1234; T03, commit def5678)", sem travessão (095/T08). `commit_of` devolve o hash
/// curto do commit de integração de uma tarefa. `None` sem `covers` no plano ou sem critérios.
pub fn traceability_section(
    spec_body: &str,
    tasks: &[Task],
    commit_of: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    traceability_section_in(Lang::PtBr, spec_body, tasks, commit_of)
}

/// [`traceability_section`] com os textos fixos no idioma dado (spec 102).
pub fn traceability_section_in(
    lang: Lang,
    spec_body: &str,
    tasks: &[Task],
    commit_of: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if !plan_declares_covers(tasks) {
        return None;
    }
    let criteria = global_criteria(spec_body);
    if criteria.is_empty() {
        return None;
    }
    let mut out = format!("{}\n", crate::i18n::text(lang, "report.traceability.title"));
    for (i, text) in criteria.iter().enumerate() {
        let refs: Vec<String> = tasks_covering(tasks, i)
            .map(|t| {
                let id = &t.frontmatter.id;
                match commit_of(id) {
                    Some(hash) => tr(lang, "report.traceability.commit", &[("id", id), ("hash", &hash)]),
                    None => tr(lang, "report.traceability.no_commit", &[("id", id)]),
                }
            })
            .collect();
        let target = if refs.is_empty() { crate::i18n::text(lang, "report.traceability.no_task") } else { refs.join("; ") };
        out.push_str(&format!("- G{}: {} ({target})\n", i + 1, snippet(text)));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_BODY: &str = "## Contexto\nx\n\n## Critérios de Aceitação Globais\n\
        - [ ] Login funciona com e-mail e senha.\n\
        - [ ] Logout limpa a sessão.\n  - sub item não conta\n\
        - [x] Um critério bem longo que passa de sessenta caracteres para ser cortado no aviso.\n\n\
        ## Outra\n- não é critério\n";

    fn task(id: &str, covers: &[&str]) -> Task {
        let list = covers.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
        let raw = format!(
            "---\nid: \"{id}\"\nspec_id: \"s\"\ntitle: \"t\"\nstatus: \"TODO\"\ndepends_on: []\nbranch: \"\"\n\
             attempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nallowed_scope: [\"src/**\"]\n\
             covers: [{list}]\n---\n## Objetivo\nx\n"
        );
        Task::parse(&raw).expect("tarefa válida")
    }

    #[test]
    fn normalize_accepts_only_g_and_positive_number() {
        assert_eq!(normalize_cover(" G3 ").as_deref(), Some("G3"));
        assert_eq!(normalize_cover("G03").as_deref(), Some("G3"));
        for bad in ["G0", "g1", "G", "G1a", "1", "T01", "G-1", "G 1", ""] {
            assert_eq!(normalize_cover(bad), None, "{bad}");
        }
    }

    #[test]
    fn global_criteria_reads_top_level_items_in_order() {
        let c = global_criteria(SPEC_BODY);
        assert_eq!(c.len(), 3, "{c:?}");
        assert_eq!(c[0], "Login funciona com e-mail e senha.");
        assert_eq!(c[1], "Logout limpa a sessão.");
        assert!(global_criteria("## Contexto\nsem critérios\n").is_empty());
        assert_eq!(global_criteria("## Critérios de Aceitação Globais\n1. um\n2) dois\n"), vec!["um", "dois"]);
    }

    #[test]
    fn plan_without_covers_has_no_warning_nor_section() {
        let tasks = vec![task("T01", &[]), task("T02", &[])];
        assert!(coverage_warnings(SPEC_BODY, &tasks).is_empty());
        assert_eq!(traceability_section(SPEC_BODY, &tasks, |_| None), None);
    }

    #[test]
    fn warns_uncovered_criterion_and_missing_id() {
        let tasks = vec![task("T01", &["G1"]), task("T02", &["G2", "G9"])];
        let w = coverage_warnings(SPEC_BODY, &tasks);
        assert_eq!(w[0], "T02: covers cita G9, que não existe");
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[1].starts_with("G3 (Um critério bem longo"), "{w:?}");
        assert!(w[1].ends_with("…) não é coberto por nenhuma tarefa"), "{w:?}");
        let full = vec![task("T01", &["G1", "G3"]), task("T02", &["G2"])];
        assert!(coverage_warnings(SPEC_BODY, &full).is_empty());
    }

    #[test]
    fn snippet_drops_bold_and_criterion_id_prefix() {
        assert_eq!(
            snippet("**G1 — Acesso e papéis:** usuários entram"),
            "Acesso e papéis: usuários entram"
        );
        assert_eq!(snippet("G1. Sem cookie, sem acesso"), "Sem cookie, sem acesso");
        assert_eq!(snippet("G2: __itálico__ aqui"), "itálico aqui");
        assert_eq!(snippet("G3 - traço"), "traço");
        assert_eq!(snippet("G4 – meia-risca"), "meia-risca");
        // O `***` da redação de segredos fica intacto.
        assert_eq!(snippet("chave *** no header"), "chave *** no header");
        // Sem separador ou sem número, nada sai.
        assert_eq!(snippet("G1 texto"), "G1 texto");
        assert_eq!(snippet("Gerar relatório"), "Gerar relatório");
    }

    #[test]
    fn traceability_and_warnings_use_clean_snippet() {
        let spec = "## Critérios de Aceitação Globais\n- **G1 — Acesso e papéis:** usuários entram.\n\
            - G2. Sem cookie, sem acesso.\n";
        let tasks = vec![task("T01", &["G1"])];
        let w = coverage_warnings(spec, &tasks);
        assert_eq!(w, vec!["G2 (Sem cookie, sem acesso.) não é coberto por nenhuma tarefa"]);
        let section = traceability_section(spec, &tasks, |_| Some("abc1234".into())).expect("seção");
        assert!(
            section.contains("- G1: Acesso e papéis: usuários entram. (T01, commit abc1234)\n"),
            "{section}"
        );
    }

    #[test]
    fn traceability_lists_tasks_commits_and_gaps() {
        let tasks = vec![task("T01", &["G1"]), task("T02", &["G1"]), task("T03", &["G9"])];
        let section = traceability_section(SPEC_BODY, &tasks, |id| (id == "T01").then(|| "abc1234".to_string()))
            .expect("seção");
        assert!(section.starts_with("## Rastreabilidade\n"), "{section}");
        assert!(
            section.contains("- G1: Login funciona com e-mail e senha. (T01, commit abc1234; T02, sem commit de integração)\n"),
            "{section}"
        );
        assert!(section.contains("- G2: Logout limpa a sessão. (sem tarefa declarada)\n"), "{section}");
        assert!(section.contains("- G3: "), "{section}");
        assert_eq!(section.lines().count(), 4);
        assert!(!section.contains('—') && !section.contains('–') && !section.contains('→'), "{section}");
    }

    /// Spec 102: textos fixos da rastreabilidade no idioma ativo na geração do relatório.
    #[test]
    fn traceability_section_in_english() {
        let tasks = vec![task("T01", &["G1"]), task("T02", &["G1"])];
        let section = traceability_section_in(Lang::En, SPEC_BODY, &tasks, |id| (id == "T01").then(|| "abc1234".to_string()))
            .expect("seção");
        assert!(section.starts_with("## Traceability\n"), "{section}");
        assert!(section.contains("T01, commit abc1234; T02, no integration commit"), "{section}");
        assert!(section.contains("(no task declared)"), "{section}");
        assert_eq!(
            traceability_section(SPEC_BODY, &tasks, |_| None),
            traceability_section_in(Lang::PtBr, SPEC_BODY, &tasks, |_| None)
        );
    }
}
