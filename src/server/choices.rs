//! Perguntas estruturadas do Arquiteto (bloco cercado ```` ```nants-choices ````), espelho em Rust de
//! `web/src/choices.ts` (spec 093 T01). Usado pelo `GET /api/attention` para saber se a última
//! resposta do Arquiteto aguarda escolhas do usuário.
//!
//! Regras iguais às do cliente:
//! - bloco: ```` ```nants-choices ```` + espaços/tabs + quebra de linha, corpo até o primeiro ```` ``` ````
//!   seguido de espaços/tabs e quebra de linha ou fim do texto;
//! - vale o ÚLTIMO bloco do texto: se ele for inválido, não há perguntas (mesmo com um válido antes);
//! - JSON `{"questions":[{"id","text","multi","options":[...]}]}`: 1 a 4 perguntas, 2 a 6 opções
//!   não vazias, `text` não vazio, `multi` booleano quando presente;
//! - `recommended` (texto ou lista) filtrado para as opções existentes, sem repetição, no máximo uma
//!   quando `multi` não é `true`; `why` só com recomendação.

use serde_json::Value;

/// Máximo de perguntas por bloco (`MAX_CHOICE_QUESTIONS`).
pub const MAX_CHOICE_QUESTIONS: usize = 4;
/// Mínimo de opções por pergunta (`MIN_CHOICE_OPTIONS`).
pub const MIN_CHOICE_OPTIONS: usize = 2;
/// Máximo de opções por pergunta (`MAX_CHOICE_OPTIONS`).
pub const MAX_CHOICE_OPTIONS: usize = 6;

const OPEN: &str = "```nants-choices";
const FENCE: &str = "```";

/// Pergunta validada (mesmos campos de `ChoiceQuestion`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceQuestion {
    pub id: String,
    pub text: String,
    pub multi: bool,
    pub options: Vec<String>,
    pub recommended: Vec<String>,
    pub why: Option<String>,
}

/// Perguntas do ÚLTIMO bloco `nants-choices` do texto; `None` sem bloco ou com o último inválido.
pub fn parse_choices(text: &str) -> Option<Vec<ChoiceQuestion>> {
    let mut questions = None;
    for body in blocks(text) {
        questions = serde_json::from_str::<Value>(body).ok().and_then(|raw| validate(&raw));
    }
    questions
}

/// Corpos dos blocos, na ordem, com a mesma varredura da regex global do cliente
/// (`/```nants-choices[ \t]*\r?\n([\s\S]*?)```[ \t]*(?:\r?\n|$)/g`).
fn blocks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(found) = text[pos..].find(OPEN) {
        let start = pos + found;
        match block_at(text, start) {
            Some((body, end)) => {
                out.push(body);
                pos = end;
            }
            // Sem casamento nesta posição: a regex tenta a partir do próximo caractere.
            None => pos = start + 1,
        }
        if pos >= text.len() {
            break;
        }
    }
    out
}

/// Casamento ancorado em `start` (início de ```` ```nants-choices ````): `(corpo, fim do casamento)`.
fn block_at(text: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = text.as_bytes();
    let mut i = start + OPEN.len();
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i = newline_end(bytes, i)?;
    let body_start = i;
    // Corpo preguiçoso: o primeiro fechamento que satisfaz o restante do padrão.
    let mut search = body_start;
    while let Some(found) = text[search..].find(FENCE) {
        let close = search + found;
        let mut j = close + FENCE.len();
        while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
            j += 1;
        }
        if j == bytes.len() {
            return Some((&text[body_start..close], j));
        }
        if let Some(end) = newline_end(bytes, j) {
            return Some((&text[body_start..close], end));
        }
        search = close + 1;
    }
    None
}

/// `\r?\n` em `i`: devolve a posição depois da quebra.
fn newline_end(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes.get(i) {
        Some(b'\n') => Some(i + 1),
        Some(b'\r') if bytes.get(i + 1) == Some(&b'\n') => Some(i + 2),
        _ => None,
    }
}

/// Texto não vazio depois de `trim` (o `trim` do JavaScript; diferenças só em brancos exóticos).
fn non_blank(s: &str) -> bool {
    !s.trim().is_empty()
}

fn validate(raw: &Value) -> Option<Vec<ChoiceQuestion>> {
    let list = raw.as_object()?.get("questions")?.as_array()?;
    if list.is_empty() || list.len() > MAX_CHOICE_QUESTIONS {
        return None;
    }
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        let item = item.as_object()?;
        let id = item.get("id")?.as_str()?;
        let text = item.get("text")?.as_str()?;
        if !non_blank(text) {
            return None;
        }
        let multi = match item.get("multi") {
            None => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return None,
        };
        let options = item.get("options")?.as_array()?;
        if options.len() < MIN_CHOICE_OPTIONS || options.len() > MAX_CHOICE_OPTIONS {
            return None;
        }
        let options: Vec<String> = options
            .iter()
            .map(|o| o.as_str().filter(|s| non_blank(s)).map(str::to_string))
            .collect::<Option<_>>()?;
        let candidates: Vec<&str> = match item.get("recommended") {
            Some(Value::String(s)) => vec![s.as_str()],
            Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let mut recommended: Vec<String> = Vec::new();
        for r in candidates {
            if options.iter().any(|o| o == r) && !recommended.iter().any(|x| x == r) {
                recommended.push(r.to_string());
            }
        }
        if !multi {
            recommended.truncate(1);
        }
        let why = item
            .get("why")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|w| !w.is_empty() && !recommended.is_empty())
            .map(str::to_string);
        out.push(ChoiceQuestion {
            id: id.to_string(),
            text: text.trim().to_string(),
            multi,
            options,
            recommended,
            why,
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    //! Paridade com `web/src/choices.test.ts`: os mesmos casos válidos e inválidos, mais os de borda
    //! da regex (fechamento falso, `\r\n`, bloco sem fechamento e último bloco inválido).
    use super::*;
    use serde_json::json;

    /// `q(n, opts)` do teste do cliente.
    fn q(n: usize, opts: &[&str]) -> Value {
        Value::Array(
            (0..n)
                .map(|i| json!({"id": format!("q{i}"), "text": format!("P{i}"), "multi": false, "options": opts}))
                .collect(),
        )
    }

    /// `block(obj)` do teste do cliente.
    fn block(obj: &Value) -> String {
        format!("Texto\n```nants-choices\n{obj}\n```\n")
    }

    fn count(text: &str) -> Option<usize> {
        parse_choices(text).map(|qs| qs.len())
    }

    #[test]
    fn extracts_valid_block() {
        assert_eq!(count(&block(&json!({"questions": q(2, &["A", "B"])}))), Some(2));
    }

    #[test]
    fn invalid_json_has_no_questions() {
        assert_eq!(count("Oi\n```nants-choices\n{nope\n```"), None);
    }

    #[test]
    fn respects_limits() {
        assert_eq!(count(&block(&json!({"questions": q(5, &["A", "B"])}))), None);
        assert_eq!(count(&block(&json!({"questions": q(1, &["A"])}))), None);
        assert_eq!(count(&block(&json!({"questions": q(1, &["1", "2", "3", "4", "5", "6", "7"])}))), None);
        assert_eq!(count(&block(&json!({"questions": q(4, &["1", "2", "3", "4", "5", "6"])}))), Some(4));
        assert_eq!(count(&block(&json!({"questions": []}))), None);
    }

    #[test]
    fn block_in_the_middle_of_text() {
        let text = format!("Antes\n```nants-choices\n{}\n```\nDepois", json!({"questions": q(1, &["A", "B"])}));
        assert_eq!(count(&text), Some(1));
    }

    #[test]
    fn uses_last_block_and_plain_text_has_none() {
        let two = block(&json!({"questions": q(1, &["A", "B"])})) + &block(&json!({"questions": q(2, &["A", "B"])}));
        assert_eq!(count(&two), Some(2));
        assert_eq!(count("só texto"), None);
        // Último bloco inválido anula o válido anterior (como o `replace` do cliente).
        let then_bad = block(&json!({"questions": q(1, &["A", "B"])})) + "```nants-choices\n{nope\n```\n";
        assert_eq!(count(&then_bad), None);
    }

    #[test]
    fn recommended_and_why() {
        let blk = |q: Value| format!("```nants-choices\n{}\n```", json!({"questions": [q]}));
        let a = parse_choices(&blk(json!({"id": "a", "text": "Q?", "options": ["x", "y"], "recommended": "y", "why": " porque "})))
            .expect("válido");
        assert_eq!(a[0].recommended, vec!["y"]);
        assert_eq!(a[0].why.as_deref(), Some("porque"));
        let b = parse_choices(&blk(json!({"id": "a", "text": "Q?", "multi": true, "options": ["x", "y"], "recommended": ["x", "z", "y"]})))
            .expect("válido");
        assert_eq!(b[0].recommended, vec!["x", "y"]);
        let c = parse_choices(&blk(json!({"id": "a", "text": "Q?", "options": ["x", "y"], "recommended": "z", "why": "w"})))
            .expect("válido");
        assert!(c[0].recommended.is_empty());
        assert_eq!(c[0].why, None);
        // Sem `multi`, só a primeira recomendação vale.
        let d = parse_choices(&blk(json!({"id": "a", "text": "Q?", "options": ["x", "y"], "recommended": ["y", "x"]})))
            .expect("válido");
        assert_eq!(d[0].recommended, vec!["y"]);
    }

    #[test]
    fn invalid_question_shapes() {
        let blk = |q: Value| format!("```nants-choices\n{}\n```", json!({"questions": [q]}));
        for bad in [
            json!({"id": 1, "text": "Q", "options": ["x", "y"]}),
            json!({"id": "a", "text": "  ", "options": ["x", "y"]}),
            json!({"id": "a", "text": "Q", "multi": "sim", "options": ["x", "y"]}),
            json!({"id": "a", "text": "Q", "options": ["x", " "]}),
            json!({"id": "a", "text": "Q", "options": ["x", 2]}),
            json!({"id": "a", "text": "Q"}),
            json!("texto"),
        ] {
            assert_eq!(count(&blk(bad.clone())), None, "{bad}");
        }
        assert_eq!(count("```nants-choices\n[]\n```"), None);
        assert_eq!(count("```nants-choices\n{\"questions\": 3}\n```"), None);
        // `multi: null` não é booleano.
        assert_eq!(count(&blk(json!({"id": "a", "text": "Q", "multi": null, "options": ["x", "y"]}))), None);
    }

    #[test]
    fn regex_edge_cases() {
        let body = json!({"questions": q(1, &["A", "B"])}).to_string();
        // `\r\n` e espaços depois das cercas.
        assert_eq!(count(&format!("```nants-choices  \r\n{body}\r\n```\t\r\nfim")), Some(1));
        // Cerca de abertura sem quebra de linha logo depois: não é bloco.
        assert_eq!(count(&format!("```nants-choices {body}\n```")), None);
        // Bloco sem fechamento: não é bloco.
        assert_eq!(count(&format!("```nants-choices\n{body}\n")), None);
        // Fechamento seguido de texto na mesma linha não fecha; o corpo vai até o próximo válido
        // (e aí o JSON fica inválido, como no cliente).
        assert_eq!(count(&format!("```nants-choices\n{body}\n```x\n```\n")), None);
        // Fechamento no fim do texto, sem quebra.
        assert_eq!(count(&format!("x ```nants-choices\n{body}\n```")), Some(1));
        // Outro bloco cercado comum não conta.
        assert_eq!(count(&format!("```json\n{body}\n```\n")), None);
    }
}
