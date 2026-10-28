//! Spec 131 T01: a spec inteira para os agentes. O briefing do Desenvolvedor, do Revisor e da
//! verificação final traz o índice da spec (títulos de qualquer nível, com o tamanho) e as seções
//! inteiras que cabem no teto, nesta ordem: as citadas pela tarefa pelo título, as Premissas e o
//! resto na ordem do texto. Uma seção nunca é cortada ao meio; o que não cabe fica só no índice, e
//! a ferramenta `read_spec_section` a devolve inteira.
//!
//! Os textos daqui vão ao modelo (briefing e resultado de ferramenta), não à pessoa.

use super::split_frontmatter;

/// Título da seção que entra logo depois das citadas.
pub const PREMISES: &str = "Premissas";
/// Nome da ferramenta que lê uma seção da spec.
pub const TOOL_NAME: &str = "read_spec_section";
/// Argumento da ferramenta que devolve o índice.
pub const LIST: &str = "list";
/// Teto da resposta da ferramenta: a seção inteira até aqui; acima, o corte numa linha avisa as
/// subseções para ler uma a uma.
pub const MAX_SECTION_BYTES: usize = 16 * 1024;

/// Uma seção da spec: cabeçalho `##` a `######` fora de bloco de código.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Nível do cabeçalho (2 = `##`).
    pub level: usize,
    pub title: String,
    /// Cabeçalho e texto próprio, até o próximo cabeçalho de qualquer nível.
    pub own: String,
    /// Cabeçalho, texto próprio e subseções, até o próximo cabeçalho de nível igual ou maior.
    pub full: String,
    /// Índice (exclusivo) da última subseção: as seções `i+1..end` são as descendentes.
    end: usize,
}

/// Nível e título de uma linha de cabeçalho (`## Título`), de 2 a 6 `#`.
fn heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(2..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    if !rest.starts_with(' ') && !rest.starts_with('\t') {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    (!title.is_empty()).then_some((hashes, title))
}

/// Corpo da spec sem o frontmatter (texto inteiro se não houver).
fn body_of(spec_md: &str) -> &str {
    split_frontmatter(spec_md).map(|(_, body)| body).unwrap_or(spec_md)
}

/// Seções da spec na ordem do texto. Cabeçalhos dentro de blocos de código não contam.
pub fn parse(spec_md: &str) -> Vec<Section> {
    let lines: Vec<&str> = body_of(spec_md).lines().collect();
    let mut fence: Option<&str> = None;
    // (linha do cabeçalho, nível, título)
    let mut heads: Vec<(usize, usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let marker = if t.starts_with("```") {
            Some("```")
        } else if t.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(m) = marker {
            fence = match fence {
                Some(open) if open == m => None,
                None => Some(m),
                other => other,
            };
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some((level, title)) = heading(line) {
            heads.push((i, level, title.to_string()));
        }
    }
    let join = |from: usize, to: usize| -> String {
        let text = lines[from..to].join("\n");
        text.trim_end().to_string()
    };
    let mut out = Vec::with_capacity(heads.len());
    for (k, (start, level, title)) in heads.iter().enumerate() {
        let own_end = heads.get(k + 1).map_or(lines.len(), |h| h.0);
        let end = heads[k + 1..]
            .iter()
            .position(|h| h.1 <= *level)
            .map_or(heads.len(), |p| k + 1 + p);
        let full_end = heads.get(end).map_or(lines.len(), |h| h.0);
        out.push(Section {
            level: *level,
            title: title.clone(),
            own: join(*start, own_end),
            full: join(*start, full_end),
            end,
        });
    }
    out
}

/// Tamanho legível para o índice.
fn size_label(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        let tenths = (bytes * 10 + 512) / 1024;
        format!("{}.{} KB", tenths / 10, tenths % 10)
    }
}

/// Espaço mínimo de uma ocorrência de título repetido em `read_spec_section`; abaixo, só a linha de origem.
const OCCURRENCE_RESERVE: usize = 256;

/// Teto do índice no briefing: acima dele, só `##` e `###`, e o resto pelo `"list"` da ferramenta.
pub const MAX_INDEX_BYTES: usize = 4 * 1024;

/// `true` se `needle` aparece em `hay` com fronteira de palavra dos dois lados (letra, dígito ou
/// `_` vizinho não conta): um título curto dentro de outra palavra não é citação.
fn contains_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(needle).any(|(at, m)| {
        let before = hay[..at].chars().next_back().is_none_or(|c| !is_word(c));
        let after = hay[at + m.len()..].chars().next().is_none_or(|c| !is_word(c));
        before && after
    })
}

/// Seções cujo título aparece no texto citado (objetivo e critérios da tarefa), sem diferenciar
/// maiúsculas e com fronteira de palavra; da mais específica (mais profunda) para a mais geral e,
/// no mesmo nível, na ordem do texto.
fn cited(sections: &[Section], cited_text: &str) -> Vec<usize> {
    let hay = cited_text.to_lowercase();
    let mut out: Vec<usize> = sections
        .iter()
        .enumerate()
        .filter(|(_, s)| contains_word(&hay, &s.title.to_lowercase()))
        .map(|(i, _)| i)
        .collect();
    out.sort_by_key(|i| std::cmp::Reverse(sections[*i].level));
    out
}

/// Seção mãe de `i` (a anterior mais próxima de nível menor que a contém); `None` no topo.
fn parent_of(sections: &[Section], i: usize) -> Option<usize> {
    (0..i).rev().find(|j| sections[*j].level < sections[i].level && sections[*j].end > i)
}

/// Seção que o mesmo briefing já entrega inteira por outro caminho (spec 131 T01): fica no índice e
/// na ferramenta, mas não entra de novo no texto. Sem `sub`, a seção inteira; com `sub`, o texto
/// próprio da seção e a subseção `sub` (ex.: as orientações do próprio papel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivered<'a> {
    pub section: &'a str,
    pub sub: Option<&'a str>,
}

/// Onde está o texto próprio de cada seção no briefing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// Abaixo, nas seções da spec.
    Below,
    /// Em outra seção do mesmo briefing.
    Elsewhere,
    /// Só pela ferramenta.
    Out,
}

/// Seções entregues por outro caminho (`true`), pelos títulos de `delivered`.
fn delivered_marks(sections: &[Section], delivered: &[Delivered]) -> Vec<bool> {
    let mut marks = vec![false; sections.len()];
    for d in delivered {
        for (i, s) in sections.iter().enumerate() {
            if !s.title.eq_ignore_ascii_case(d.section) {
                continue;
            }
            match d.sub {
                None => marks[i..s.end].iter_mut().for_each(|m| *m = true),
                Some(sub) => {
                    if let Some(k) = (i + 1..s.end).find(|k| sections[*k].title.eq_ignore_ascii_case(sub)) {
                        marks[i] = true;
                        marks[k..sections[k].end].iter_mut().for_each(|m| *m = true);
                    }
                }
            }
        }
    }
    marks
}

/// Escolhe as seções (pelo texto próprio, inteiro) que cabem em `max` bytes, na ordem: citadas,
/// Premissas e o resto na ordem do texto. Cada candidata entra com as subseções se couber tudo;
/// senão, entram as partes que cabem, uma a uma, sempre inteiras. As já entregues por outro
/// caminho não entram.
fn select(sections: &[Section], cited_text: &str, max: usize, delivered: &[Delivered]) -> Vec<Place> {
    let mut place: Vec<Place> = delivered_marks(sections, delivered)
        .iter()
        .map(|e| if *e { Place::Elsewhere } else { Place::Out })
        .collect();
    let mut used = 0usize;
    let mut take = |root: usize, place: &mut Vec<Place>| {
        let range = root..sections[root].end;
        let missing: usize =
            range.clone().filter(|i| place[*i] == Place::Out).map(|i| sections[i].own.len() + 2).sum();
        if used + missing <= max {
            for i in range {
                if place[i] == Place::Out {
                    place[i] = Place::Below;
                }
            }
            used += missing;
            return;
        }
        for i in range {
            let cost = sections[i].own.len() + 2;
            if place[i] == Place::Out && used + cost <= max {
                place[i] = Place::Below;
                used += cost;
            }
        }
    };
    let mut order = cited(sections, cited_text);
    order.extend(sections.iter().position(|s| s.title.eq_ignore_ascii_case(PREMISES)));
    order.extend(0..sections.len());
    for root in order {
        take(root, &mut place);
    }
    place
}

/// Índice da spec, um título por linha com o nível, o tamanho e, com `place`, onde o texto está.
/// Só os níveis até `max_level`; as marcas cobrem a seção inteira, com as subseções.
fn render_index(sections: &[Section], place: Option<&[Place]>, max_level: usize) -> String {
    let mut out = String::new();
    for (i, s) in sections.iter().enumerate() {
        if s.level > max_level {
            continue;
        }
        let indent = "  ".repeat(s.level.saturating_sub(2));
        let mark = match place {
            None => String::new(),
            Some(p) => {
                let sub = &p[i..s.end];
                let below = sub.contains(&Place::Below);
                let elsewhere = sub.contains(&Place::Elsewhere);
                let out = sub.contains(&Place::Out);
                match (below, elsewhere, out) {
                    (true, false, false) => " [inteira abaixo]".to_string(),
                    (false, true, false) => " [entregue em outra seção do briefing]".to_string(),
                    (true, true, false) => " [abaixo e em outra seção do briefing]".to_string(),
                    (false, false, _) => format!(" [fora do briefing: {TOOL_NAME}]"),
                    _ => format!(" [em parte no briefing; o resto: {TOOL_NAME}]"),
                }
            }
        };
        out.push_str(&format!(
            "{indent}- {} {} ({}){mark}\n",
            "#".repeat(s.level),
            s.title,
            size_label(s.full.len())
        ));
    }
    out.trim_end().to_string()
}

/// Rebaixa um nível só o cabeçalho da seção (a 1ª linha de `own`), para não se confundir com as
/// seções do briefing (`## Objetivo`); o corpo chega idêntico à spec, inclusive blocos cercados.
fn demote(own: &str) -> String {
    format!("#{own}")
}

/// Contexto da spec para o briefing: o índice e as seções inteiras que cabem em `max` bytes
/// (citadas por `cited_text`, Premissas e o resto), sem repetir as `delivered`. Vazio sem seções.
pub fn briefing_context(spec_md: &str, cited_text: &str, max: usize, delivered: &[Delivered]) -> String {
    let sections = parse(spec_md);
    if sections.is_empty() {
        return String::new();
    }
    let place = select(&sections, cited_text, max, delivered);
    let mut index = render_index(&sections, Some(&place), 6);
    let mut short = "";
    if index.len() > MAX_INDEX_BYTES {
        index = render_index(&sections, Some(&place), 3);
        short = " Índice longo: aqui só ## e ###; os demais níveis estão no \"list\" da ferramenta.";
    }
    let mut out = format!(
        "Índice da spec (título e tamanho de cada seção). As marcadas [fora do briefing] e [em parte no briefing] \
         não couberam aqui: leia a seção inteira com {TOOL_NAME} (title = título exato; \"{LIST}\" = o índice completo).{short}\n{index}"
    );
    let body: Vec<String> = sections
        .iter()
        .zip(&place)
        .filter(|(_, p)| **p == Place::Below)
        .map(|(s, _)| demote(&s.own))
        .collect();
    if !body.is_empty() {
        out.push_str("\n\nSeções da spec (inteiras):\n\n");
        out.push_str(&body.join("\n\n"));
    }
    out
}

/// Resultado de `read_spec_section`: com `"list"`, o índice; com um título (sem diferenciar
/// maiúsculas, com ou sem os `#`), a seção inteira com as subseções. Título repetido devolve todas
/// as ocorrências, cada uma com a linha de origem (a seção mãe). O total fica em
/// [`MAX_SECTION_BYTES`]; acima, corta numa linha e lista as subseções para ler uma a uma.
/// `Err` com o índice quando o título não é uma seção desta spec.
pub fn read_section(spec_md: &str, title: &str) -> Result<String, String> {
    let sections = parse(spec_md);
    if sections.is_empty() {
        return Err("a spec desta execução não tem seções".into());
    }
    let wanted = title.trim().trim_start_matches('#').trim();
    if wanted.eq_ignore_ascii_case(LIST) {
        return Ok(format!("Índice da spec:\n{}", render_index(&sections, None, 6)));
    }
    let lower = wanted.to_lowercase();
    let found: Vec<usize> = (0..sections.len()).filter(|i| sections[*i].title.to_lowercase() == lower).collect();
    if found.is_empty() {
        return Err(format!(
            "\"{wanted}\" não é uma seção da spec desta execução; use um título exato do índice:\n{}",
            render_index(&sections, None, 6)
        ));
    }
    if let [i] = found[..] {
        return Ok(clip_section(&sections, i, MAX_SECTION_BYTES));
    }
    let total = found.len();
    let mut out = String::new();
    for (n, i) in found.iter().enumerate() {
        let origin = match parent_of(&sections, *i) {
            Some(p) => format!("em {} {}", "#".repeat(sections[p].level), sections[p].title),
            None => "no topo da spec".to_string(),
        };
        // A origem vai curta, para a linha caber sempre.
        let origin: String = origin.chars().take(OCCURRENCE_RESERVE / 2).collect();
        let head = format!("[ocorrência {} de {total}, {origin}]\n", n + 1);
        // Partes iguais do que resta entre as ocorrências que faltam; o que uma não usa fica para as seguintes.
        let share = MAX_SECTION_BYTES.saturating_sub(out.len()) / (total - n);
        let room = share.saturating_sub(head.len() + 2);
        if room < OCCURRENCE_RESERVE {
            let omitted = format!("[ocorrência {} de {total}, {origin}: acima do limite de {} KB]\n", n + 1, MAX_SECTION_BYTES / 1024);
            if out.len() + omitted.len() <= MAX_SECTION_BYTES {
                out.push_str(&omitted);
            }
            continue;
        }
        out.push_str(&head);
        out.push_str(&clip_section(&sections, *i, room));
        out.push_str("\n\n");
    }
    Ok(out.trim_end().to_string())
}

/// Seção `i` inteira se couber em `max` bytes; senão, cortada numa linha com a lista das subseções.
fn clip_section(sections: &[Section], i: usize, max: usize) -> String {
    let s = &sections[i];
    if s.full.len() <= max {
        return s.full.clone();
    }
    let note = format!("[seção acima do limite de {} KB; leia as subseções uma a uma:]\n", MAX_SECTION_BYTES / 1024);
    let subs: Vec<String> = sections[i + 1..s.end].iter().map(|c| format!("- {}", c.title)).collect();
    let tail = format!("{note}{}", subs.join("\n"));
    let budget = max.saturating_sub(tail.len() + 1);
    let mut out = String::new();
    for line in s.full.lines() {
        if out.len() + line.len() + 1 > budget {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&tail);
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST9: &str = include_str!("testdata/test9/spec.md");

    #[test]
    fn index_lists_every_heading_level_with_its_size() {
        let ctx = briefing_context(TEST9, "", 4096, &[]);
        for line in [
            "- ## Contexto e Problema (",
            "- ## Abordagem Técnica (",
            "  - ### CLI and configuration contract (",
            "  - ### Monthly report contract (",
            "- ## Premissas (",
            "  - ### Revisor (",
            "- ## Persistência da instalação (",
        ] {
            assert!(ctx.contains(line), "faltou no índice: {line}\n{ctx}");
        }
        assert!(ctx.contains("KB)"), "{ctx}");
        assert!(!ctx.contains("test_command: python"), "o frontmatter não entra: {ctx}");
    }

    #[test]
    fn a_cited_section_after_the_cap_arrives_whole() {
        let sections = parse(TEST9);
        let report = sections.iter().find(|s| s.title == "Monthly report contract").expect("seção");
        // A seção começa depois dos primeiros 4 KB do texto: no corte antigo ela não chegava.
        let offset = TEST9.find("### Monthly report contract").expect("cabeçalho");
        assert!(offset > 4096);
        let objective = "Implement `report` following the monthly report contract section of spec.md.";
        let ctx = briefing_context(TEST9, objective, 4096, &[]);
        assert!(ctx.contains(&demote(&report.own)), "{ctx}");
        assert!(ctx.contains("### Monthly report contract (") && ctx.contains("[inteira abaixo]"));
        // As Premissas vêm logo depois das citadas.
        let premises = sections.iter().find(|s| s.title == PREMISES).expect("premissas");
        assert!(ctx.contains(&demote(&premises.own)));
        // Sem citação, ela não cabe e fica só no índice, com o aviso de como ler.
        let plain = briefing_context(TEST9, "", 4096, &[]);
        assert!(!plain.contains("Top expenses are the 10 most negative"));
        assert!(plain.contains(&format!("### Monthly report contract ({}) [fora do briefing: {TOOL_NAME}]", size_label(report.full.len()))));
        // Pela ferramenta, chega inteira.
        let read = read_section(TEST9, "monthly REPORT contract").expect("lida");
        assert_eq!(read, report.full);
        assert!(read.contains("Markdown and CSV totals, rankings and uncategorized IDs must agree exactly."));
    }

    #[test]
    fn a_section_is_never_cut_in_the_middle() {
        for max in [0, 300, 1000, 2500, 4096, 6000, 9000, 20000] {
            for cited in ["", "Internal transfers", "Parsing and atomic import and Premissas"] {
                let ctx = briefing_context(TEST9, cited, max, &[]);
                let body = ctx.split_once("Seções da spec (inteiras):\n\n").map_or("", |(_, b)| b);
                let used: usize = parse(TEST9)
                    .iter()
                    .filter(|s| body.contains(&demote(&s.own)))
                    .map(|s| s.own.len() + 2)
                    .sum();
                assert!(used <= max, "teto {max} excedido: {used}");
                for s in parse(TEST9) {
                    let head = format!("#{}", s.own.lines().next().unwrap_or_default());
                    if body.lines().any(|l| l == head) {
                        assert!(body.contains(&demote(&s.own)), "seção {} cortada (teto {max})", s.title);
                    }
                }
            }
        }
    }

    #[test]
    fn order_is_cited_then_premises_then_the_rest() {
        let sections = parse(TEST9);
        let own = |t: &str| sections.iter().find(|s| s.title == t).map(|s| s.own.len() + 2).unwrap_or(0);
        // Só cabem a citada e as Premissas.
        let max = own("Internal transfers") + own(PREMISES);
        let ctx = briefing_context(TEST9, "see internal transfers", max, &[]);
        assert!(ctx.contains("Eligible pairs have opposite nonzero"));
        assert!(ctx.contains("The request is sufficiently complete"));
        assert!(!ctx.contains("A household needs a local"), "o resto não cabe");
        // Com folga, o resto entra na ordem do texto, e a saída segue a ordem da spec.
        let all = briefing_context(TEST9, "", 64 * 1024, &[]);
        let a = all.find("A household needs a local").expect("contexto");
        let b = all.find("Eligible pairs have opposite").expect("transferências");
        let c = all.find("The request is sufficiently complete").expect("premissas");
        assert!(a < b && b < c);
        assert!(!all.contains("[fora do briefing:") && !all.contains("[em parte no briefing;"));
    }

    #[test]
    fn sections_delivered_elsewhere_stay_in_the_index_and_the_tool_only() {
        let delivered = [
            Delivered { section: "Orientações por papel", sub: Some("Desenvolvedor") },
            Delivered { section: "critérios de aceitação globais", sub: None },
        ];
        let ctx = briefing_context(TEST9, "", 64 * 1024, &delivered);
        assert!(ctx.contains("- ## Critérios de Aceitação Globais (999 B) [entregue em outra seção do briefing]"), "{ctx}");
        assert!(ctx.contains("  - ### Desenvolvedor (562 B) [entregue em outra seção do briefing]"), "{ctx}");
        assert!(ctx.contains("  - ### Revisor (535 B) [inteira abaixo]"), "{ctx}");
        assert!(ctx.contains("- ## Orientações por papel (1.1 KB) [abaixo e em outra seção do briefing]"), "{ctx}");
        assert!(!ctx.contains("G1: The Docker workspace") && !ctx.contains("Use Decimal only at parsing"), "{ctx}");
        assert!(ctx.contains("Check overlap multiplicity"), "a outra subseção continua: {ctx}");
        // Mesmo citada, não se repete; a ferramenta continua lendo.
        let cited = briefing_context(TEST9, "see Critérios de Aceitação Globais", 64 * 1024, &delivered);
        assert!(!cited.contains("G1: The Docker workspace"));
        assert!(read_section(TEST9, "Critérios de Aceitação Globais").expect("lida").contains("G1: The Docker workspace"));
        // Título ausente não marca nada.
        let none = briefing_context(TEST9, "", 64 * 1024, &[Delivered { section: "Inexistente", sub: None }]);
        assert_eq!(none, briefing_context(TEST9, "", 64 * 1024, &[]));
    }

    /// F-01: só o cabeçalho da seção é rebaixado; um modelo Markdown cercado chega idêntico.
    #[test]
    fn fenced_headings_inside_a_section_arrive_intact() {
        let md = "## Formato\nUse este modelo:\n```\n## dentro\ntexto\n```\nfim\n## Outra\nz\n";
        let ctx = briefing_context(md, "", 4096, &[]);
        assert!(ctx.contains("### Formato\nUse este modelo:\n```\n## dentro\ntexto\n```\nfim"), "{ctx}");
        assert!(!ctx.contains("### dentro"), "{ctx}");
    }

    /// F-02: título repetido devolve todas as ocorrências, inteiras e com a origem.
    #[test]
    fn a_repeated_title_returns_every_occurrence_with_its_origin() {
        let md = "## API\na\n### Testes\nprimeira\n## Interface\nb\n### Testes\nsegunda ocorrência\n";
        // Só a primeira cabe no briefing; a segunda fica fora e chega pela ferramenta.
        let ctx = briefing_context(md, "", 40, &[]);
        assert!(ctx.contains("primeira") && !ctx.contains("segunda ocorrência"), "{ctx}");
        let read = read_section(md, "testes").expect("lidas");
        assert!(read.contains("[ocorrência 1 de 2, em ## API]\n### Testes\nprimeira"), "{read}");
        assert!(read.contains("[ocorrência 2 de 2, em ## Interface]\n### Testes\nsegunda ocorrência"), "{read}");
        // O total respeita o teto, mesmo com ocorrências grandes.
        let big = format!("## A\n### X\n{0}\n## B\n### X\n{0}\n", "linha de texto\n".repeat(1500));
        let read = read_section(&big, "X").expect("lidas");
        assert!(read.len() <= MAX_SECTION_BYTES && read.contains("[ocorrência 2 de 2, em ## B]\n### X\nlinha"), "{}", read.len());
    }

    /// (a) Índice acima do teto: só `##` e `###`, e o aviso do `"list"`.
    #[test]
    fn a_long_index_keeps_only_the_first_two_levels() {
        let mut md = String::new();
        for i in 0..60 {
            md.push_str(&format!("## Seção {i}\n### Parte {i}\n#### Detalhe profundo {i}\nx\n"));
        }
        let ctx = briefing_context(&md, "", 1024, &[]);
        assert!(ctx.contains("- ## Seção 59") && ctx.contains("  - ### Parte 59"), "{ctx}");
        assert!(!ctx.contains("#### Detalhe profundo 59 ("), "{ctx}");
        assert!(ctx.contains("os demais níveis estão no \"list\" da ferramenta"));
        assert!(read_section(&md, "list").expect("índice").contains("#### Detalhe profundo 59"));
        assert!(!briefing_context(TEST9, "", 4096, &[]).contains("Índice longo"));
    }

    /// (b) e (c): citação com fronteira de palavra, da seção mais específica para a mais geral.
    #[test]
    fn citations_respect_word_boundaries_and_prefer_the_most_specific() {
        let md = "## Dados\ngeral\n### Contrato\nespecífico\n## Uso\nu\n";
        assert!(contains_word("segue o contrato.", "contrato"));
        assert!(!contains_word("os usos do app", "uso") && !contains_word("abuso", "uso"));
        // "Uso" dentro de "usos" não é citação: com teto curto, ela não entra.
        let ctx = briefing_context(md, "os usos do app", 16, &[]);
        assert!(!ctx.contains("### Uso\nu"), "{ctx}");
        assert!(briefing_context(md, "veja Uso", 16, &[]).contains("### Uso\nu"));
        // Citadas "Dados" e "Contrato": a mais profunda entra primeiro.
        let sections = parse(md);
        assert_eq!(cited(&sections, "Dados e Contrato"), vec![1, 0]);
        let ctx = briefing_context(md, "Dados e Contrato", 26, &[]);
        assert!(ctx.contains("#### Contrato\nespecífico") && !ctx.contains("### Dados\ngeral"), "{ctx}");
    }

    #[test]
    fn the_tool_refuses_what_is_not_a_spec_section() {
        for bad in ["../../etc/passwd", "tasks/T03.md", "Objetivo", "", "Nonexistent"] {
            let err = read_section(TEST9, bad).expect_err(bad);
            assert!(err.contains("não é uma seção") && err.contains("### Internal transfers"), "{err}");
        }
        let list = read_section(TEST9, "list").expect("índice");
        assert!(list.contains("## Abordagem Técnica") && !list.contains("[inteira"));
        // Um `##` vem com as subseções; com os `#`, também acha.
        let approach = read_section(TEST9, "## Abordagem Técnica").expect("seção");
        assert!(approach.contains("### Internal transfers") && approach.contains("### Monthly report contract"));
        assert!(!approach.contains("## Premissas"));
        assert!(read_section("sem cabeçalho", "x").is_err());
    }

    #[test]
    fn headings_in_code_blocks_and_oversized_sections() {
        let md = "## A\n```\n## not a heading\n```\ntexto\n## B\nb\n";
        let s = parse(md);
        assert_eq!(s.iter().map(|x| x.title.as_str()).collect::<Vec<_>>(), ["A", "B"]);
        assert!(s[0].own.contains("## not a heading"));
        let big = format!("## Grande\n{}\n### Parte\nfim\n", "linha longa de texto\n".repeat(1500));
        let out = read_section(&big, "Grande").expect("lida");
        assert!(out.len() <= MAX_SECTION_BYTES && out.contains("- Parte") && out.contains("leia as subseções"), "{}", out.len());
    }
}
