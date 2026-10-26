//! Ordem de execução das tarefas e renumeração na aprovação (spec 128 T04).
//!
//! - [`execution_order`] é a única fonte da ordem: topológica pelo `depends_on` e, nos empates, pelo
//!   id atual ([`compare_ids`]: prefixo e número, `T9` antes de `T10`). Dependência desconhecida é
//!   ignorada (a aprovação já a recusa). Com ciclo, não há ordem: devolve as tarefas do ciclo.
//! - [`renumber_map`]: quando a ordem difere dos ids, o mapa antigo para novo. Os ids novos são os
//!   mesmos ids do plano, redistribuídos na ordem de execução: o conjunto de arquivos não muda e só
//!   o conteúdo troca de lugar. Só ids `T` + 2 dígitos são renumerados; com qualquer outro formato
//!   (por exemplo `T100`), o plano fica como está e a tela só exibe a ordem.
//! - [`rewrite_refs`]: troca simultânea (sem cadeia) das referências `\bT\d{2}\b`, fora de blocos de
//!   código cercados (``` ou ~~~) e de trechos entre crases da mesma linha. Ids de 3 dígitos não
//!   casam (o dígito seguinte não é fronteira de palavra).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::{Spec, Task};

/// Compara ids de tarefa: prefixo não numérico, depois o número final, depois o texto.
pub fn compare_ids(a: &str, b: &str) -> Ordering {
    fn split(s: &str) -> (&str, Option<u64>) {
        let prefix = s.trim_end_matches(|c: char| c.is_ascii_digit());
        (prefix, s[prefix.len()..].parse().ok())
    }
    let (pa, na) = split(a);
    let (pb, nb) = split(b);
    pa.cmp(pb).then(na.cmp(&nb)).then_with(|| a.cmp(b))
}

/// Ordem de execução de `tasks` (`(id, depends_on)`). `Err` com as tarefas que formam ciclo, na
/// ordem dos ids. Ids repetidos contam uma vez (a primeira ocorrência).
pub fn execution_order(tasks: &[(String, Vec<String>)]) -> Result<Vec<String>, Vec<String>> {
    let mut deps: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut ids: Vec<&str> = Vec::new();
    for (id, _) in tasks {
        if !deps.contains_key(id.as_str()) {
            deps.insert(id.as_str(), HashSet::new());
            ids.push(id.as_str());
        }
    }
    let known: HashSet<&str> = ids.iter().copied().collect();
    let mut seen: HashSet<&str> = HashSet::new();
    for (id, list) in tasks {
        if !seen.insert(id.as_str()) {
            continue;
        }
        if let Some(entry) = deps.get_mut(id.as_str()) {
            entry.extend(list.iter().map(String::as_str).filter(|d| known.contains(d)));
        }
    }
    let mut done: HashSet<&str> = HashSet::new();
    let mut order: Vec<String> = Vec::with_capacity(ids.len());
    loop {
        let next = ids
            .iter()
            .copied()
            .filter(|id| !done.contains(id) && deps.get(id).is_some_and(|d| d.iter().all(|x| done.contains(x))))
            .min_by(|a, b| compare_ids(a, b));
        match next {
            Some(id) => {
                done.insert(id);
                order.push(id.to_string());
            }
            None => break,
        }
    }
    if order.len() == ids.len() {
        return Ok(order);
    }
    // Restam as tarefas do ciclo e as que dependem dele: corta, em rodadas, as que nenhuma outra
    // restante usa (as de baixo do ciclo). Sobra só o ciclo.
    let mut left: HashSet<&str> = ids.iter().copied().filter(|id| !done.contains(id)).collect();
    loop {
        let used: HashSet<&str> = left.iter().flat_map(|id| deps.get(id).into_iter().flatten().copied()).filter(|d| left.contains(d)).collect();
        let before = left.len();
        left.retain(|id| used.contains(id));
        if left.len() == before {
            break;
        }
    }
    let mut cycle: Vec<String> = left.into_iter().map(str::to_string).collect();
    cycle.sort_by(|a, b| compare_ids(a, b));
    Err(cycle)
}

/// `true` para `T` seguido de exatamente 2 dígitos.
fn is_two_digit_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 3 && b[0] == b'T' && b[1].is_ascii_digit() && b[2].is_ascii_digit()
}

/// Mapa antigo para novo quando `order` difere dos ids; `None` se já está em ordem ou se algum id
/// não é `T` + 2 dígitos. Só as tarefas que mudam de id entram no mapa.
pub fn renumber_map(order: &[String]) -> Option<BTreeMap<String, String>> {
    if order.is_empty() || !order.iter().all(|id| is_two_digit_id(id)) {
        return None;
    }
    let mut sorted = order.to_vec();
    sorted.sort_by(|a, b| compare_ids(a, b));
    let map: BTreeMap<String, String> = order
        .iter()
        .zip(&sorted)
        .filter(|(old, new)| old != new)
        .map(|(old, new)| (old.clone(), new.clone()))
        .collect();
    (!map.is_empty()).then_some(map)
}

/// Texto do mapa na ordem dos ids novos: `T08→T02, T10→T03`.
pub fn map_text(map: &BTreeMap<String, String>) -> String {
    let mut pairs: Vec<(&String, &String)> = map.iter().collect();
    pairs.sort_by(|a, b| compare_ids(a.1, b.1));
    pairs.iter().map(|(old, new)| format!("{old}→{new}")).collect::<Vec<_>>().join(", ")
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Troca simultânea de cada `\bT\d{2}\b` presente no mapa, sem olhar para código.
pub fn replace_ids(text: &str, map: &BTreeMap<String, String>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts = c == 'T'
            && (i == 0 || !is_word(chars[i - 1]))
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
            && chars.get(i + 2).is_some_and(char::is_ascii_digit)
            && chars.get(i + 3).is_none_or(|n| !is_word(*n));
        if starts {
            let token: String = chars[i..i + 3].iter().collect();
            if let Some(new) = map.get(&token) {
                out.push_str(new);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Uma linha fora de bloco cercado: troca fora dos trechos entre crases (corrida de N crases até a
/// próxima corrida de exatamente N na mesma linha; sem fechamento, as crases são texto).
fn rewrite_line(line: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut plain = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('`') {
        plain.push_str(&rest[..start]);
        let after = &rest[start..];
        let run = after.len() - after.trim_start_matches('`').len();
        let body = &after[run..];
        let close = find_run(body, run);
        match close {
            Some(end) => {
                out.push_str(&replace_ids(&plain, map));
                plain.clear();
                out.push_str(&after[..run + end + run]);
                rest = &body[end + run..];
            }
            None => {
                plain.push_str(&after[..run]);
                rest = body;
            }
        }
    }
    plain.push_str(rest);
    out.push_str(&replace_ids(&plain, map));
    out
}

/// Posição (em bytes) da próxima corrida de exatamente `n` crases em `text`.
fn find_run(text: &str, n: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let mut j = i;
            while j < bytes.len() && bytes[j] == b'`' {
                j += 1;
            }
            if j - i == n {
                return Some(i);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

/// Marcador de cerca (``` ou ~~~, 3 ou mais) no início da linha (até 3 espaços), com o caractere e o
/// tamanho.
fn fence_of(line: &str) -> Option<(char, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = rest.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = rest.len() - rest.trim_start_matches(ch).len();
    (len >= 3).then_some((ch, len))
}

/// Troca as referências do Markdown `text`, ignorando blocos cercados e trechos entre crases.
pub fn rewrite_refs(text: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open: Option<(char, usize)> = None;
    for piece in text.split_inclusive('\n') {
        let line = piece.trim_end_matches(['\n', '\r']);
        let ending = &piece[line.len()..];
        match (open, fence_of(line)) {
            (None, Some(fence)) => {
                open = Some(fence);
                out.push_str(piece);
            }
            (Some((ch, len)), Some((c, l))) if c == ch && l >= len && line.trim().trim_start_matches(c).is_empty() => {
                open = None;
                out.push_str(piece);
            }
            (Some(_), _) => out.push_str(piece),
            (None, None) => {
                out.push_str(&rewrite_line(line, map));
                out.push_str(ending);
            }
        }
    }
    out
}

/// Tarefa com o id, o `depends_on`, o título e o corpo traduzidos pelo mapa.
pub fn renumber_task(task: &Task, map: &BTreeMap<String, String>) -> Task {
    let mut out = task.clone();
    let swap = |id: &String| map.get(id).cloned().unwrap_or_else(|| id.clone());
    out.frontmatter.id = swap(&task.frontmatter.id);
    out.frontmatter.depends_on = task.frontmatter.depends_on.iter().map(swap).collect();
    out.frontmatter.title = replace_ids(&task.frontmatter.title, map);
    out.body = rewrite_refs(&task.body, map);
    out
}

/// Spec com as referências do título e do corpo traduzidas e a seção `section` (já pronta, com o
/// título `## …`) no fim do corpo.
pub fn renumber_spec(spec: &Spec, map: &BTreeMap<String, String>, section: &str) -> Spec {
    let mut out = spec.clone();
    out.frontmatter.title = replace_ids(&spec.frontmatter.title, map);
    let mut body = rewrite_refs(&spec.body, map);
    let trimmed = body.trim_end().len();
    body.truncate(trimmed);
    body.push_str("\n\n");
    body.push_str(section.trim_end());
    body.push('\n');
    out.body = body;
    out
}

/// Registro JSON (`plan-review.json`, `plan-changes.json`) com cada texto traduzido: valor igual a
/// um id antigo vira o novo; nos demais textos, as referências são trocadas.
pub fn renumber_json(value: &mut Value, map: &BTreeMap<String, String>) {
    match value {
        Value::String(s) => {
            *s = match map.get(s.as_str()) {
                Some(new) => new.clone(),
                None => replace_ids(s, map),
            };
        }
        Value::Array(items) => items.iter_mut().for_each(|v| renumber_json(v, map)),
        Value::Object(fields) => fields.values_mut().for_each(|v| renumber_json(v, map)),
        _ => {}
    }
}

/// Dados de teste do Test 6 (spec 128 T04).
#[cfg(test)]
pub(crate) mod test6 {
    /// Tarefas do Test 6 (cópias de `~/NantsProjects/test-6`, spec 001-game-night), em ordem de id.
    pub const TEST6: [&str; 15] = [
        include_str!("testdata/test6/T01.md"),
        include_str!("testdata/test6/T02.md"),
        include_str!("testdata/test6/T03.md"),
        include_str!("testdata/test6/T04.md"),
        include_str!("testdata/test6/T05.md"),
        include_str!("testdata/test6/T06.md"),
        include_str!("testdata/test6/T07.md"),
        include_str!("testdata/test6/T08.md"),
        include_str!("testdata/test6/T09.md"),
        include_str!("testdata/test6/T10.md"),
        include_str!("testdata/test6/T11.md"),
        include_str!("testdata/test6/T12.md"),
        include_str!("testdata/test6/T13.md"),
        include_str!("testdata/test6/T14.md"),
        include_str!("testdata/test6/T15.md"),
    ];

    /// Ordem de execução do Test 6: T01, T08, T10, T02, T03…
    pub const TEST6_ORDER: [&str; 15] =
        ["T01", "T08", "T10", "T02", "T03", "T04", "T05", "T13", "T14", "T12", "T15", "T11", "T06", "T09", "T07"];
}

#[cfg(test)]
mod tests {
    use super::test6::{TEST6, TEST6_ORDER};
    use super::*;

    fn plan(items: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        items.iter().map(|(id, deps)| (id.to_string(), deps.iter().map(|d| d.to_string()).collect())).collect()
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn the_test6_order_is_reproduced_and_renumbered() {
        let tasks: Vec<(String, Vec<String>)> = TEST6
            .iter()
            .map(|raw| Task::parse(raw).expect("tarefa"))
            .map(|t| (t.frontmatter.id, t.frontmatter.depends_on))
            .collect();
        let order = execution_order(&tasks).expect("sem ciclo");
        assert_eq!(order, TEST6_ORDER);
        let m = renumber_map(&order).expect("fora de ordem");
        assert_eq!(m.get("T08").map(String::as_str), Some("T02"));
        assert_eq!(m.get("T10").map(String::as_str), Some("T03"));
        assert_eq!(m.get("T07").map(String::as_str), Some("T15"));
        assert!(!m.contains_key("T01"), "T01 continua T01");
        // Depois de renumerar, a ordem coincide com os ids: nada mais a fazer.
        let renumbered: Vec<(String, Vec<String>)> = TEST6
            .iter()
            .map(|raw| renumber_task(&Task::parse(raw).expect("tarefa"), &m))
            .map(|t| (t.frontmatter.id, t.frontmatter.depends_on))
            .collect();
        let again = execution_order(&renumbered).expect("sem ciclo");
        assert_eq!(renumber_map(&again), None);
        let t02 = renumber_task(&Task::parse(TEST6[1]).expect("T02"), &m);
        assert_eq!(t02.frontmatter.id, "T04");
        assert_eq!(t02.frontmatter.depends_on, ["T02", "T03"]);
        assert!(t02.body.contains("Do not introduce play/history tables before T07."), "{}", t02.body);
    }

    #[test]
    fn ties_follow_the_id_and_numbers_compare_as_numbers() {
        let order = execution_order(&plan(&[("T03", &[]), ("T01", &[]), ("T02", &[])])).expect("ordem");
        assert_eq!(order, ["T01", "T02", "T03"]);
        assert_eq!(compare_ids("T9", "T10"), Ordering::Less);
        assert_eq!(compare_ids("T10", "T09"), Ordering::Greater);
        // Empate entre prontas: a de menor id vai antes, mesmo listada depois.
        let order = execution_order(&plan(&[("T04", &["T01"]), ("T03", &["T01"]), ("T01", &[]), ("T02", &["T04"])])).expect("ordem");
        assert_eq!(order, ["T01", "T03", "T04", "T02"]);
    }

    #[test]
    fn a_cycle_has_no_order_and_reports_only_its_members() {
        let err = execution_order(&plan(&[("T01", &[]), ("T02", &["T03"]), ("T03", &["T02"]), ("T04", &["T02"])])).expect_err("ciclo");
        assert_eq!(err, ["T02", "T03"]);
        let err = execution_order(&plan(&[("T01", &["T01"])])).expect_err("ciclo em si");
        assert_eq!(err, ["T01"]);
        // Dependência desconhecida não trava a ordem.
        assert_eq!(execution_order(&plan(&[("T01", &["T99"])])).expect("ordem"), ["T01"]);
    }

    #[test]
    fn the_map_only_exists_when_the_order_differs_and_ids_have_two_digits() {
        assert_eq!(renumber_map(&["T01".into(), "T02".into()]), None);
        // Lacunas em ordem: nada a renumerar.
        assert_eq!(renumber_map(&["T01".into(), "T05".into()]), None);
        assert_eq!(renumber_map(&["T01".into(), "T03".into(), "T02".into()]), Some(map(&[("T03", "T02"), ("T02", "T03")])));
        assert_eq!(renumber_map(&["T100".into(), "T02".into()]), None, "3 dígitos ficam fora");
        assert_eq!(renumber_map(&["A2".into(), "A1".into()]), None);
    }

    #[test]
    fn replacement_is_simultaneous_and_respects_word_boundaries() {
        let m = map(&[("T02", "T03"), ("T03", "T02")]);
        assert_eq!(replace_ids("T02 antes de T03; T023, XT02, T02_a, T02.", &m), "T03 antes de T02; T023, XT02, T02_a, T03.");
        assert_eq!(replace_ids("T02→T03", &m), "T03→T02");
    }

    #[test]
    fn code_blocks_and_backticks_are_kept() {
        let m = map(&[("T08", "T02")]);
        let text = "Depende da T08 e de `T08` e ``a `T08` b``.\n```\nT08 no código\n```\n~~~~\nT08\n~~~~\nFim T08 com ` solta.\n";
        let out = rewrite_refs(text, &m);
        assert_eq!(
            out,
            "Depende da T02 e de `T08` e ``a `T08` b``.\n```\nT08 no código\n```\n~~~~\nT08\n~~~~\nFim T02 com ` solta.\n"
        );
    }

    #[test]
    fn json_records_are_translated() {
        let mut value = serde_json::json!({"signature": "abc", "at": "2026-10-26T12:00:00Z", "issues": [{"id": "P1", "task": "T08", "text": "T08 depende de T10"}]});
        renumber_json(&mut value, &map(&[("T08", "T02"), ("T10", "T03")]));
        assert_eq!(value["issues"][0]["task"], "T02");
        assert_eq!(value["issues"][0]["text"], "T02 depende de T03");
        assert_eq!(value["at"], "2026-10-26T12:00:00Z");
        assert_eq!(value["issues"][0]["id"], "P1");
    }

    #[test]
    fn map_text_follows_the_new_ids() {
        assert_eq!(map_text(&map(&[("T08", "T02"), ("T02", "T04"), ("T10", "T03")])), "T08→T02, T10→T03, T02→T04");
    }
}
