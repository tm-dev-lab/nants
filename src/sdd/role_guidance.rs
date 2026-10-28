//! Seção opcional `## Orientações por papel` do spec.md (spec 061/T02): orientações específicas da
//! demanda, escritas pelo Arquiteto, com subseções `### Desenvolvedor` e `### Revisor`.

/// Título da seção no spec.md.
pub const ROLE_GUIDANCE_SECTION: &str = "Orientações por papel";
/// Subseções reconhecidas.
pub const ROLE_DEVELOPER: &str = "Desenvolvedor";
pub const ROLE_REVIEWER: &str = "Revisor";
/// Limite de cada subseção (1,5 KiB).
pub const MAX_ROLE_GUIDANCE_BYTES: usize = 1536;

/// Linhas internas da seção (entre `## Orientações por papel` e o próximo `## `); `None` se ausente.
fn section_lines(spec_md: &str) -> Option<Vec<&str>> {
    let header = format!("## {ROLE_GUIDANCE_SECTION}");
    let mut found = false;
    let mut out = Vec::new();
    for line in spec_md.lines() {
        if !found {
            found = line.trim_end() == header;
            continue;
        }
        if line.starts_with("## ") {
            break;
        }
        out.push(line);
    }
    found.then_some(out)
}

/// Texto (aparado) da subseção `### role`; `None` se a seção ou a subseção não existem.
pub fn role_subsection(spec_md: &str, role: &str) -> Option<String> {
    let lines = section_lines(spec_md)?;
    let header = format!("### {role}");
    let mut found = false;
    let mut out = String::new();
    for line in lines {
        if !found {
            found = line.trim_end() == header;
            continue;
        }
        if line.starts_with("### ") {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    found.then(|| out.trim().to_string())
}

/// Corta em `max` bytes numa fronteira de caractere, marcando o corte.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[...truncado]", text[..end].trim_end())
}

/// Orientação do papel para o briefing: vazia sem seção/subseção; truncada a [`MAX_ROLE_GUIDANCE_BYTES`].
pub fn role_guidance_of(spec_md: &str, role: &str) -> String {
    role_subsection(spec_md, role).map(|t| clip(&t, MAX_ROLE_GUIDANCE_BYTES)).unwrap_or_default()
}

/// `true` se [`role_guidance_of`] entrega a subseção do papel inteira, sem corte (spec 131 T01: o
/// contexto da spec não a repete).
pub fn delivered_whole(spec_md: &str, role: &str) -> bool {
    role_subsection(spec_md, role).is_some_and(|t| !t.is_empty() && t.len() <= MAX_ROLE_GUIDANCE_BYTES)
}

/// Valida a seção ao gravar o spec: ausente é válido; presente exige ao menos uma subseção
/// reconhecida com conteúdo e cada subseção reconhecida não vazia e dentro do limite.
pub fn validate(spec_md: &str) -> bool {
    if section_lines(spec_md).is_none() {
        return true;
    }
    let mut any = false;
    for role in [ROLE_DEVELOPER, ROLE_REVIEWER] {
        if let Some(t) = role_subsection(spec_md, role) {
            if t.is_empty() || t.len() > MAX_ROLE_GUIDANCE_BYTES {
                return false;
            }
            any = true;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "# T\n## Premissas\n- a\n## Orientações por papel\n### Desenvolvedor\nUse `lein test`.\n\n### Revisor\nVeja o .gitignore.\n## Outra\nz\n";

    #[test]
    fn extracts_only_the_role_subsection() {
        assert_eq!(role_guidance_of(SPEC, ROLE_DEVELOPER), "Use `lein test`.");
        assert_eq!(role_guidance_of(SPEC, ROLE_REVIEWER), "Veja o .gitignore.");
        assert!(!role_guidance_of(SPEC, ROLE_DEVELOPER).contains("gitignore"));
    }

    #[test]
    fn absent_section_or_role_is_empty_and_valid() {
        assert_eq!(role_guidance_of("# T\n## Premissas\n- a\n", ROLE_DEVELOPER), "");
        assert!(validate("# T\n## Premissas\n- a\n"));
        let only_dev = "## Orientações por papel\n### Desenvolvedor\nx\n";
        assert_eq!(role_guidance_of(only_dev, ROLE_REVIEWER), "");
        assert!(validate(only_dev));
    }

    #[test]
    fn validation_bounds_each_subsection() {
        assert!(validate(SPEC));
        assert!(!validate("## Orientações por papel\n\n"));
        assert!(!validate("## Orientações por papel\ntexto solto\n"));
        assert!(!validate("## Orientações por papel\n### Desenvolvedor\n\n### Revisor\nx\n"));
        let big = format!("## Orientações por papel\n### Revisor\n{}\n", "x".repeat(MAX_ROLE_GUIDANCE_BYTES + 1));
        assert!(!validate(&big));
        let edge = format!("## Orientações por papel\n### Revisor\n{}\n", "x".repeat(MAX_ROLE_GUIDANCE_BYTES));
        assert!(validate(&edge));
    }

    #[test]
    fn guidance_truncates_on_char_boundary() {
        let spec = format!("## Orientações por papel\n### Revisor\n{}\n", "ç".repeat(1000));
        let t = role_guidance_of(&spec, ROLE_REVIEWER);
        assert!(t.ends_with("[...truncado]") && t.len() <= MAX_ROLE_GUIDANCE_BYTES + 20, "{}", t.len());
    }
}
