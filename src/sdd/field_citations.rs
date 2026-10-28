//! Critério que cita um campo da spec que não existe (spec 131 T04). No Test 9 um critério dizia
//! "the spec run_command imports…" numa spec sem `run_command`: o Revisor deu ✓ e o relatório
//! inventou um comando. A detecção é uma função única, [`missing_fields_cited`]: o plano a usa nos
//! avisos (`changed_plan_warnings`) e a execução (T05) a reaproveita na revisão.

use super::{Settings, SpecMeta};

/// Campos opcionais do frontmatter da spec que um critério pode citar, com a regra de citação:
/// `true` vale o identificador solto no texto (nome com `_`, que não é palavra comum); `false` só
/// conta entre crases (`checks`, `references`: palavras comuns em critério).
const FIELDS: [(&str, bool); 14] = [
    ("test_command", true),
    ("run_command", true),
    ("run_url", true),
    ("checks", false),
    ("formatters", false),
    ("failure_artifacts", true),
    ("screenshots", false),
    ("references", false),
    ("environment", false),
    ("full_test_command", true),
    ("install_command", true),
    ("lockfiles", true),
    ("not_in_sandbox", true),
    ("nothing_to_run", true),
];

/// Campos da spec que `text` (um critério) cita pelo identificador literal e que não existem ou
/// estão vazios, na ordem de [`FIELDS`]. Os campos que a aprovação herda do `settings.yaml`
/// (`test_command`, `checks`, formatadores, artefatos, níveis de teste, instalação, travas e
/// ambiente) contam como existentes se o settings os tiver; `settings` ausente não ajuda.
pub fn missing_fields_cited(text: &str, spec: &SpecMeta, settings: Option<&Settings>) -> Vec<&'static str> {
    FIELDS
        .iter()
        .filter(|(name, bare)| cites(text, name, *bare))
        .map(|(name, _)| *name)
        .filter(|name| !present(name, spec, settings))
        .collect()
}

/// O campo existe com valor (na spec ou, quando herdável, no settings).
fn present(name: &str, spec: &SpecMeta, settings: Option<&Settings>) -> bool {
    let some = |v: &Option<String>| v.as_deref().is_some_and(|t| !t.trim().is_empty());
    let list = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|l| !l.is_empty());
    let from_settings = |f: &dyn Fn(&Settings) -> bool| settings.is_some_and(f);
    match name {
        "test_command" => spec.valid_test_command().is_some() || from_settings(&|s| !s.test_command.trim().is_empty()),
        "run_command" => some(&spec.run_command),
        "run_url" => some(&spec.run_url),
        "checks" => spec.checks.as_ref().is_some_and(|c| !c.is_empty()) || from_settings(&|s| !s.checks.is_empty()),
        "formatters" => {
            spec.formatters.as_ref().is_some_and(|f| !f.is_empty()) || from_settings(&|s| !s.formatters.is_empty())
        }
        "failure_artifacts" => list(&spec.failure_artifacts) || from_settings(&|s| !s.failure_artifacts.is_empty()),
        "screenshots" => list(&spec.screenshots) || from_settings(&|s| !s.screenshots.is_empty()),
        "references" => list(&spec.references),
        "environment" => some(&spec.environment) || settings.is_some(),
        "full_test_command" => some(&spec.full_test_command) || from_settings(&|s| some(&s.full_test_command)),
        "install_command" => some(&spec.install_command) || from_settings(&|s| some(&s.install_command)),
        "lockfiles" => list(&spec.lockfiles) || from_settings(&|s| !s.lockfiles.is_empty()),
        "not_in_sandbox" => some(&spec.not_in_sandbox),
        "nothing_to_run" => some(&spec.nothing_to_run),
        _ => true,
    }
}

/// `text` cita `name`: entre crases (só o nome, ou o nome seguido de `:`, `=` ou espaço) ou, se
/// `bare`, solto com fronteira de identificador dos dois lados.
fn cites(text: &str, name: &str, bare: bool) -> bool {
    let in_code = text.split('`').skip(1).step_by(2).any(|span| {
        let span = span.trim();
        span.strip_prefix(name)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '=', ' ']))
    });
    in_code || (bare && cites_bare(text, name))
}

fn cites_bare(text: &str, name: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(i, _)| {
        let before = text[..i].chars().next_back().is_none_or(|c| !ident(c));
        let after = text[i + name.len()..].chars().next().is_none_or(|c| !ident(c));
        before && after
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdd::Spec;

    fn spec(extra: &str) -> SpecMeta {
        let text = format!(
            "---\nid: \"001-x\"\ntitle: \"X\"\nstatus: \"DRAFT\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-27T00:00:00Z\"\n{extra}---\ncorpo\n"
        );
        Spec::parse(&text).expect("spec").frontmatter
    }

    #[test]
    fn criterion_citing_an_absent_or_empty_field_is_detected() {
        // O caso do Test 9: a spec não declara run_command e o critério o cita.
        let meta = spec("");
        let text = "The spec run_command imports the checking example and creates both reports.";
        assert_eq!(missing_fields_cited(text, &meta, None), vec!["run_command"]);
        // Vazio equivale a ausente.
        let empty = spec("run_command: \"  \"\nrun_url: \"\"\n");
        assert_eq!(missing_fields_cited("Abre o `run_url` depois do run_command.", &empty, None), vec!["run_command", "run_url"]);
        // Declarado: nada a avisar.
        let declared = spec("run_command: \"python -m app demo\"\n");
        assert!(missing_fields_cited(text, &declared, None).is_empty());
    }

    #[test]
    fn only_the_literal_identifier_counts() {
        let meta = spec("");
        // Parte de outro identificador não conta; palavra comum solta também não.
        for text in [
            "my_run_command_helper roda.",
            "The checks pass and the references are listed.",
            "The environment is documented.",
            "Run the command shown in the README.",
        ] {
            assert!(missing_fields_cited(text, &meta, None).is_empty(), "{text}");
        }
        // Entre crases, a palavra comum é o campo.
        assert_eq!(missing_fields_cited("Os `checks` da spec passam.", &meta, None), vec!["checks"]);
        assert_eq!(missing_fields_cited("Declara `references: [a.png]`.", &meta, None), vec!["references"]);
    }

    #[test]
    fn fields_inherited_from_settings_count_as_present() {
        let meta = spec("");
        let settings = Settings::parse("base_branch: main\nmerge_strategy: squash\ntest_command: \"cargo test\"\n").expect("settings");
        let text = "O test_command passa e o `environment` é docker.";
        assert_eq!(missing_fields_cited(text, &meta, None), vec!["test_command", "environment"]);
        assert!(missing_fields_cited(text, &meta, Some(&settings)).is_empty());
        // run_command só existe na spec: o settings não o supre.
        assert_eq!(missing_fields_cited("run_command mostra o app", &meta, Some(&settings)), vec!["run_command"]);
    }
}
