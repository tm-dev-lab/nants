//! Redator único do Nanty (spec 140 T04, C-09).
//!
//! O [`SecretSet`] só conhece os segredos da configuração, por igualdade exata. Uma senha num log ou
//! um token no `.env` de um projeto passam por ele. Aqui ficam os padrões, num lugar só, e todo texto
//! que sai do Nanty ou fica guardado por ele passa por [`scrub`]: a leitura de projeto (na saída das
//! ferramentas), `remember`, o resumo, a resposta final e o texto de cartão (pelo `format::clean`).
//! [`patterns`] é a mesma coisa sem a configuração, para quem não a tem à mão.
//!
//! **Cobertura:** PEM, URL com credencial, `Bearer`, atribuição em nome sensível (`KEY=valor`),
//! prefixos de chaves comuns (também colados a outro texto antes deles, spec 140 T10), JWT, token do
//! Telegram e cadeias de alta entropia.
//!
//! **Falso positivo:** é aceitável, com uma exceção: o Nanty fala de hashes de commit, UUIDs e ids o
//! tempo todo. Hexadecimal nunca conta como alta entropia, e ids curtos ficam abaixo do tamanho mínimo.
//! A redação não é o controle de injeção: o prompt "não repita segredos" não conta como controle.
//!
//! Sem regex, como o `SecretSet`: varredura por bytes, cortando só em fronteiras ASCII.

use crate::server::events::SecretSet;

/// O que fica no lugar do segredo (o mesmo do `SecretSet`).
const MASK: &str = "***";
const PEM_OPEN: &str = "-----BEGIN ";
const PEM_CLOSE: &str = "-----END ";
const PEM_DASHES: &str = "-----";

/// Prefixos de chaves comuns e o tamanho mínimo do que vem depois.
const KEY_PREFIXES: &[(&str, usize)] = &[
    ("sk-", 16),
    ("gsk_", 20),
    ("xai-", 20),
    ("pplx-", 20),
    ("nvapi-", 16),
    ("hf_", 20),
    ("r8_", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("github_pat_", 20),
    ("glpat-", 16),
    ("npm_", 20),
    ("pypi-", 20),
    ("xoxa-", 10),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxr-", 10),
    ("xoxs-", 10),
    ("xapp-", 10),
    ("AIza", 30),
    ("ya29.", 20),
    ("sk_live_", 12),
    ("rk_live_", 12),
    ("pk_live_", 12),
    ("sk_test_", 12),
    ("rk_test_", 12),
    ("pk_test_", 12),
    ("whsec_", 16),
    ("dop_v1_", 20),
    ("shpat_", 16),
    ("shpss_", 16),
    ("SG.", 20),
];
/// Chaves de acesso da AWS: o prefixo e 16 caracteres.
const AWS_PREFIXES: [&str; 2] = ["AKIA", "ASIA"];
const AWS_TAIL: usize = 16;
/// Nomes sensíveis: o nome da atribuição termina num destes.
const SENSITIVE_NAMES: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "_pass",
    "_pwd",
    "senha",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "accesskey",
    "access_key",
    "secretkey",
    "secret_key",
    "privatekey",
    "private_key",
    "accountkey",
    "credential",
    "credentials",
    "authorization",
    "connectionstring",
    "connection_string",
];
/// Maior palavra depois de dois-pontos numa frase comum: acima disso, é senha de letras coladas.
const PROSE_MAX_LETTERS: usize = 20;
/// Valores de atribuição que não são segredo.
const PLAIN_VALUES: &[&str] = &["null", "none", "nil", "true", "false", "undefined", "redacted", "empty"];
/// Alta entropia: tamanho mínimo da cadeia.
const ENTROPY_MIN_LEN: usize = 24;
/// Bits por caractere a partir dos quais a cadeia é tratada como aleatória. Fica abaixo do que uma
/// cadeia curta alcança: 24 caracteres em base36 dão de 3,4 a 4,4 bits. Quem separa nome de sorteio é
/// o [`wordy`].
const ENTROPY_BITS: f64 = 3.5;
/// Nome feito de palavras (medido, ver [`wordy`]): parte mínima coberta por palavras, em quintos;
/// grupos de dígitos, no máximo; pares de letras comuns, em quartos.
const WORDY_MIN_COVER_FIFTHS: usize = 3;
const WORDY_MAX_DIGIT_GROUPS: usize = 3;
const WORDY_MIN_COMMON_FOURTHS: usize = 3;
/// Pares de letras comuns em português e em inglês.
const COMMON_PAIRS: &str = "th he in er an re on at en nd ti es or te of ed is it al ar st to nt ng se ha as ou io le ve \
co me de hi ri ro ic ne ea ra ce li ch ll be ma si om ur ca el ta la ns di fo ho pe ec pr no ct us ac ot il tr ly nc et ut \
ss so rs un lo wa ge ie wh ee wi em ad ol rt po we na ul ni ts mo ow pa im mi ai sh ir su id os iv ia am fi ci vi pl ig tu \
ev ld ry mp fe bl ab gh ty op wo sa ay ex ke fr oo av ag if ap gr od bo sp rd do uc bu ei ov by rm ep tt oc fa ef cu rn sc \
gi da yo cr cl du ga qu ue ff ba ey ls va um pp ua up lu go ht ru ug ds lt pi rc rr eg au ck ew mu br bi pt ak pu ui rg ib \
tl ny ki rk ys ob mm fu ph og ms ye ud mb ip ub oi rl gu dr hr cc tw ft wn nu af hu nn eo vo rv nf xp gn sm fl iz ok nl my \
gl aw ju oa eq sy sl ps jo lf nv je nk kn gs dy hy ze ks xt bs ik dd bj lk ax oy ao uo";

/// [`COMMON_PAIRS`] como tabela, para a conferência custar um passo por par.
static COMMON_TABLE: [bool; 676] = {
    let bytes = COMMON_PAIRS.as_bytes();
    let mut table = [false; 676];
    let mut at = 0;
    while at + 1 < bytes.len() {
        table[(bytes[at] - b'a') as usize * 26 + (bytes[at + 1] - b'a') as usize] = true;
        at += 3;
    }
    table
};

/// O texto sem os segredos conhecidos e sem o que tem cara de segredo. Chame antes de qualquer corte,
/// para nunca sobrar um pedaço.
pub fn scrub(secrets: &SecretSet, text: &str) -> String {
    patterns(&secrets.scrub_exact(text))
}

/// O texto sem o que tem cara de segredo.
pub fn patterns(text: &str) -> String {
    let text = pem(text);
    let text = url_credentials(&text);
    let text = bearer(&text);
    let text = assignments(&text);
    let text = known_tokens(&text);
    high_entropy(&text)
}

/// Blocos PEM, do início ao fim. Sem o fim, até a linha em branco ou o fim do texto.
fn pem(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    // Sem fim daqui em diante, nenhuma abertura seguinte acha um: procurar de novo seria quadrático.
    let mut closes = true;
    while let Some(start) = rest.find(PEM_OPEN) {
        out.push_str(&rest[..start]);
        out.push_str(MASK);
        let block = &rest[start..];
        let close = if closes { block.find(PEM_CLOSE) } else { None };
        closes = close.is_some();
        rest = match close {
            Some(end) => {
                let after = &block[end + PEM_CLOSE.len()..];
                after.find(PEM_DASHES).map_or("", |i| &after[i + PEM_DASHES.len()..])
            }
            None => block.find("\n\n").map_or("", |i| &block[i..]),
        };
    }
    out.push_str(rest);
    out
}

/// O que vem antes do `@` num endereço com esquema: `https://user:senha@host` vira `https://***@host`.
fn url_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | '<' | '>' | '\\' | '`')).unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(i) if i > 0 => {
                out.push_str(MASK);
                out.push_str(&authority[i..]);
            }
            _ => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// O valor depois de `Bearer` parece credencial, e não a palavra seguinte de uma frase.
fn credential_shaped(token: &str) -> bool {
    token.len() >= 16 || (token.len() >= 8 && token.bytes().any(|b| b.is_ascii_digit()))
}

/// O token depois de `Bearer`, em qualquer caixa.
fn bearer(text: &str) -> String {
    const WORD: &str = "bearer";
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut search) = (0, 0);
    while let Some(found) = lower[search..].find(WORD) {
        let start = search + found;
        let after = start + WORD.len();
        search = after;
        if start > 0 && is_token_byte(bytes[start - 1]) {
            continue;
        }
        let gap = bytes[after..].iter().take_while(|b| matches!(**b, b' ' | b'\t')).count();
        let from = after + gap;
        let end = from + bytes[from..].iter().take_while(|b| is_token_byte(**b)).count();
        if gap == 0 || !credential_shaped(&text[from..end]) {
            continue;
        }
        out.push_str(&text[copied..from]);
        out.push_str(MASK);
        copied = end;
        search = end;
    }
    out.push_str(&text[copied..]);
    out
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

/// Nome de variável sensível: termina num nome da lista, ou tem a forma `ALGO_KEY` de uma variável de
/// ambiente. `max_tokens` e `token_count` não contam: o nome precisa terminar nele.
fn sensitive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let env_key = name.len() > 4 && name.ends_with("_KEY") && !name.bytes().any(|b| b.is_ascii_lowercase());
    env_key || SENSITIVE_NAMES.iter().any(|n| lower.ends_with(n))
}

/// O valor merece a máscara: nem vazio, nem referência, nem palavra de configuração.
fn secret_value(name: &str, value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    let reference = value.starts_with(['$', '<', '{', '%']) || lower.starts_with("env:");
    let masked = value.bytes().all(|b| b == b'*');
    // Número curto não é token: é a contagem de tokens de uma conversa.
    let count = name.to_ascii_lowercase().ends_with("token") && value.len() < 9 && value.bytes().all(|b| b.is_ascii_digit());
    value.len() >= 4 && !reference && !masked && !count && !PLAIN_VALUES.contains(&lower.as_str())
}

/// Onde o valor de uma atribuição começa e termina, a partir do byte depois do separador. O valor
/// entre aspas vai até a outra aspa; `Authorization` vai até o fim da linha, com o esquema junto.
fn value_span(bytes: &[u8], from: usize, whole_line: bool) -> (usize, usize) {
    let mut start = from;
    while bytes.get(start).is_some_and(|b| matches!(*b, b' ' | b'\t')) {
        start += 1;
    }
    // A aspa do JSON escapado (`\"`) conta como aspa.
    if bytes.get(start) == Some(&b'\\') && matches!(bytes.get(start + 1), Some(b'"' | b'\'')) {
        start += 1;
    }
    if let Some(quote) = bytes.get(start).copied().filter(|b| matches!(*b, b'"' | b'\'')) {
        let open = start + 1;
        let len = bytes[open..].iter().take_while(|b| **b != quote && **b != b'\n' && **b != b'\\').count();
        return (open, open + len);
    }
    let stop = |b: u8| match whole_line {
        true => matches!(b, b'\n' | b'\r' | b'"' | b'\'' | b'\\'),
        false => b.is_ascii_whitespace() || matches!(b, b';' | b',' | b'&' | b'"' | b'\'' | b'\\' | b')' | b']' | b'}' | b'<' | b'>' | b'`'),
    };
    let len = bytes[start..].iter().take_while(|b| !stop(**b)).count();
    (start, start + len)
}

/// Dois-pontos numa frase comum: "falhou por causa da senha: incorreta". O nome vem depois de outra
/// palavra na mesma linha, sem aspas, e o valor é uma palavra só de letras, de tamanho de palavra
/// ([`PROSE_MAX_LETTERS`]). Com `=`, aspas, início de linha ou
/// valor com outro sinal, é atribuição.
fn prose(text: &str, name_start: usize, name_end: usize, sep: usize, value: &str, quoted: bool) -> bool {
    let bytes = text.as_bytes();
    let before = bytes[..name_start].iter().rev().find(|b| !matches!(**b, b' ' | b'\t'));
    let after_word = name_start > 0
        && matches!(bytes[name_start - 1], b' ' | b'\t')
        && before.is_some_and(|b| b.is_ascii_alphanumeric() || !b.is_ascii());
    let word = value.trim_end_matches(['.', '!', '?']);
    let size = word.chars().count();
    bytes[sep] == b':' && name_end == sep && !quoted && after_word && (1..=PROSE_MAX_LETTERS).contains(&size) && word.chars().all(char::is_alphabetic)
}

/// `KEY=valor` e `chave: valor` em nomes sensíveis, em env, YAML, JSON, URL e string de conexão.
fn assignments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    while at < bytes.len() {
        if !matches!(bytes[at], b'=' | b':') || matches!(bytes.get(at + 1), Some(b'=')) {
            at += 1;
            continue;
        }
        let mut end = at;
        while end > 0 && matches!(bytes[end - 1], b' ' | b'"' | b'\'' | b'\\') {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && is_name_byte(bytes[start - 1]) {
            start -= 1;
        }
        let name = &text[start..end];
        if !sensitive(name) {
            at += 1;
            continue;
        }
        let (from, to) = value_span(bytes, at + 1, name.eq_ignore_ascii_case("authorization"));
        let quoted = from > 0 && matches!(bytes[from - 1], b'"' | b'\'');
        let value = &text[from..to];
        // Fora da máscara, o valor também fica para trás: reler cada `:` dele seria quadrático.
        if secret_value(name, value) && !prose(text, start, end, at, value, quoted) {
            out.push_str(&text[copied..from]);
            out.push_str(MASK);
            copied = to;
        }
        at = to.max(at + 1);
    }
    out.push_str(&text[copied..]);
    out
}

/// Um token de chave conhecida ou um JWT.
fn known_token(run: &str) -> bool {
    let prefixed = KEY_PREFIXES.iter().any(|(prefix, tail)| run.strip_prefix(prefix).is_some_and(|rest| rest.len() >= *tail));
    let aws = AWS_PREFIXES.iter().any(|prefix| {
        run.strip_prefix(prefix).is_some_and(|rest| rest.len() == AWS_TAIL && rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
    });
    prefixed || aws || is_jwt(run)
}

/// Maior resto de uma chave colada a outro texto, em bytes: as chaves reais ficam bem abaixo disso, e o
/// teto deixa a busca linear num trecho hostil cheio de prefixos.
const GLUED_MAX_TAIL: usize = 256;

/// Onde começa uma chave de prefixo conhecido colada a letras, dígitos ou `_` antes dela
/// (`xxxghp_...`), indo até o fim do trecho (spec 140 T10). O resto precisa ter a forma da chave: o
/// tamanho do prefixo, só letras, dígitos, `-` e `_`, letras e dígitos juntos, sem ser hexadecimal
/// (hash) nem nome feito de palavras ([`wordy`]). Assim, `risk-assessment` e identificadores com `sk`,
/// `ghp` ou `xox` no meio continuam como estão.
fn glued_start(run: &str) -> Option<usize> {
    let window = run.len().saturating_sub(GLUED_MAX_TAIL + 16);
    let shaped = |rest: &str, min: usize| {
        let bytes = rest.as_bytes();
        (min..=GLUED_MAX_TAIL).contains(&rest.len())
            && bytes.iter().all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_'))
            && bytes.iter().any(u8::is_ascii_digit)
            && bytes.iter().any(u8::is_ascii_alphabetic)
            && !bytes.iter().all(|b| b.is_ascii_hexdigit() || *b == b'-')
            && !wordy(rest)
    };
    let mut best: Option<usize> = None;
    for (prefix, min) in KEY_PREFIXES {
        for (i, _) in run.match_indices(prefix).filter(|(i, _)| *i > 0 && *i >= window) {
            if !run.is_char_boundary(i) {
                continue;
            }
            if shaped(&run[i + prefix.len()..], *min) {
                best = Some(best.map_or(i, |b| b.min(i)));
                break;
            }
        }
    }
    for prefix in AWS_PREFIXES {
        let at = run.len().saturating_sub(prefix.len() + AWS_TAIL);
        if at > 0 && run.is_char_boundary(at) && run[at..].starts_with(prefix) {
            let rest = &run[at + prefix.len()..];
            if rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) && rest.bytes().any(|b| b.is_ascii_digit()) {
                best = Some(best.map_or(at, |b| b.min(at)));
            }
        }
    }
    best
}

/// Três partes (ou duas, sem assinatura) separadas por ponto, começando pelo cabeçalho JSON em base64.
fn is_jwt(run: &str) -> bool {
    let parts: Vec<&str> = run.split('.').collect();
    run.starts_with("eyJ") && run.len() >= 30 && (2..=3).contains(&parts.len()) && parts.iter().all(|p| !p.is_empty())
}

/// Token do bot do Telegram: `<número>:<35 caracteres>`.
fn telegram_tail(bytes: &[u8], digits: &str, colon: usize) -> Option<usize> {
    const TAIL: usize = 30;
    if !(8..=12).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) || bytes.get(colon) != Some(&b':') {
        return None;
    }
    let from = colon + 1;
    let len = bytes[from..].iter().take_while(|b| b.is_ascii_alphanumeric() || matches!(**b, b'_' | b'-')).count();
    (len >= TAIL).then_some(from + len)
}

/// Prefixos de chaves, JWT e token do Telegram, em cada trecho sem espaço nem pontuação.
fn known_tokens(text: &str) -> String {
    let in_run = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.');
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    while at < bytes.len() {
        if !in_run(bytes[at]) {
            at += 1;
            continue;
        }
        let end = at + bytes[at..].iter().take_while(|b| in_run(**b)).count();
        // O ponto final da frase fica de fora.
        let run = text[at..end].trim_end_matches('.');
        // A chave colada a outro texto (spec 140 T10): o que vem antes do prefixo fica.
        let (from, cut) = if known_token(run) {
            (at, Some(at + run.len()))
        } else if let Some(start) = glued_start(run) {
            (at + start, Some(at + run.len()))
        } else {
            (at, telegram_tail(bytes, run, end))
        };
        if let Some(cut) = cut {
            out.push_str(&text[copied..from]);
            out.push_str(MASK);
            copied = cut;
            at = cut;
        } else {
            at = end;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Bits por caractere (Shannon).
fn entropy(text: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in text.bytes() {
        counts[usize::from(b)] += 1;
    }
    let total = f64::from(u32::try_from(text.len()).unwrap_or(u32::MAX));
    counts.iter().filter(|c| **c > 0).map(|c| f64::from(*c) / total).map(|p| -p * p.log2()).sum()
}

/// A cadeia é um nome feito de palavras, como `ShouldReturn404WhenUserNotFound`, e não um sorteio. Só
/// achar trechos de letras não basta: em base36 aleatório, as letras vêm em trechos longos e cobrem a
/// maior parte da cadeia. O que separa é o conteúdo dos trechos. Valem as três condições juntas:
/// palavras (`Abc` ou `abc`; numa cadeia de uma caixa só, a caixa não conta) cobrem 3/5 do tamanho, há
/// no máximo 3 grupos de dígitos e 3/4 dos pares de letras são comuns. Medido em cadeias aleatórias e
/// em nomes reais: nomes ficam acima de 0,84 de pares comuns, sorteios perto de 0,5, e a cobertura e os
/// grupos de dígitos tiram o resto. O teste de mil cadeias prende o resultado.
fn wordy(run: &str) -> bool {
    let has = |f: fn(&u8) -> bool| run.as_bytes().iter().any(f);
    let folded = if has(u8::is_ascii_lowercase) && has(u8::is_ascii_uppercase) { run.to_string() } else { run.to_ascii_lowercase() };
    let bytes = folded.as_bytes();
    let (mut at, mut covered, mut pairs, mut common) = (0, 0, 0, 0);
    while at < bytes.len() {
        let start = at + usize::from(bytes[at].is_ascii_uppercase());
        let lower = bytes[start..].iter().take_while(|b| b.is_ascii_lowercase()).count();
        if lower < 2 {
            at += 1;
            continue;
        }
        let word = &bytes[at..start + lower];
        for pair in word.to_ascii_lowercase().windows(2) {
            pairs += 1;
            common += usize::from(COMMON_TABLE[usize::from(pair[0] - b'a') * 26 + usize::from(pair[1] - b'a')]);
        }
        covered += word.len();
        at += word.len();
    }
    let digit_groups = bytes.windows(2).filter(|w| !w[0].is_ascii_digit() && w[1].is_ascii_digit()).count() + usize::from(bytes[0].is_ascii_digit());
    covered * 5 >= bytes.len() * WORDY_MIN_COVER_FIFTHS && digit_groups <= WORDY_MAX_DIGIT_GROUPS && common * 4 >= pairs * WORDY_MIN_COMMON_FOURTHS
}

/// Cadeia com cara de aleatória. Hexadecimal nunca conta (hash de commit, UUID, ids), nem caminho com
/// muitas barras, nem nome feito de palavras.
fn random_shaped(run: &str) -> bool {
    let bytes = run.as_bytes();
    let count = |f: fn(&u8) -> bool| bytes.iter().filter(|b| f(b)).count();
    let (lower, upper, digits) = (count(u8::is_ascii_lowercase), count(u8::is_ascii_uppercase), count(u8::is_ascii_digit));
    let hex = bytes.iter().all(|b| b.is_ascii_hexdigit() || *b == b'-');
    let path = count(|b| *b == b'/') > run.len() / 16;
    if run.len() < ENTROPY_MIN_LEN || hex || path {
        return false;
    }
    let plain = lower + upper + digits == run.len();
    let mixed = lower >= 2 && upper >= 2;
    let single_case = plain && (lower == 0 || upper == 0) && digits >= 2;
    (mixed || single_case) && entropy(run) >= ENTROPY_BITS && !wordy(run)
}

/// Cadeias de alta entropia.
fn high_entropy(text: &str) -> String {
    let in_run = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+' | b'/' | b'=');
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut at) = (0, 0);
    while at < bytes.len() {
        if !in_run(bytes[at]) {
            at += 1;
            continue;
        }
        let end = at + bytes[at..].iter().take_while(|b| in_run(**b)).count();
        if random_shaped(&text[at..end]) {
            out.push_str(&text[copied..at]);
            out.push_str(MASK);
            copied = end;
        }
        at = end;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Todos os valores são sintéticos e montados em tempo de execução: nenhum literal com cara de chave.
    fn tail(n: usize) -> String {
        "aB3dE6gH9kM2pQ5sT8vX1zC4fJ7nR0uW".chars().cycle().take(n).collect()
    }

    fn clean_of(secret: &str, around: &str) {
        let out = patterns(&around.replace("{}", secret));
        assert!(!out.contains(secret), "vazou: {out}");
        assert!(out.contains(MASK), "{out}");
    }

    #[test]
    fn prefixes_of_common_keys() {
        for (prefix, size) in KEY_PREFIXES {
            clean_of(&format!("{prefix}{}", tail(*size + 4)), "a chave é {}, viu.");
        }
        for prefix in AWS_PREFIXES {
            clean_of(&format!("{prefix}{}", "ABCD2345EFGH6789"), "chave {} aqui");
        }
        assert_eq!(patterns("risk-assessment-of-the-whole-plan e task-attempts-exhausted-reason"), "risk-assessment-of-the-whole-plan e task-attempts-exhausted-reason");
        assert_eq!(patterns("[\"sk-\", \"ghp_abc\"]"), "[\"sk-\", \"ghp_abc\"]", "prefixo sozinho não é chave");
    }

    /// Spec 140 T10: a chave colada a letras, dígitos ou `_` antes dela também sai, no começo, no meio e
    /// no fim do texto, e o que vem antes do prefixo fica.
    #[test]
    fn a_key_glued_to_other_text_is_masked() {
        for (prefix, size) in KEY_PREFIXES {
            let key = format!("{prefix}{}", tail(*size + 4));
            for glue in ["xxx", "x".repeat(1490).as_str(), "42", "abc_", "_"] {
                for around in ["{}", "a chave {} no log", "fim: {}"] {
                    let text = around.replace("{}", &format!("{glue}{key}"));
                    let out = patterns(&text);
                    assert!(!out.contains(&key[..key.len().min(prefix.len() + 6)]), "vazou {prefix}: {out}");
                    assert!(out.contains(&format!("{glue}***")), "o que vem antes fica: {out}");
                }
            }
        }
        assert_eq!(patterns(&format!("xxx{}{} fim", "ghp_", tail(24))), "xxx*** fim");
        assert_eq!(patterns(&format!("valorAKIA{}", "ABCD2345EFGH6789")), "valor***");
    }

    /// Texto comum com `sk`, `ghp` ou `xox` no meio, identificadores e hashes: nada muda.
    #[test]
    fn glued_prefixes_in_plain_text_stay() {
        // `{s}`, `{g}` e `{x}` viram `s`, `g` e `x` em tempo de execução: nenhum literal com cara de chave.
        for raw in [
            "risk-assessment-of-the-whole-plan e task-attempts-exhausted-reason",
            "a{s}k-v2-endpoint-for-the-users e de{s}k-2024-planning-for-the-whole-team",
            "disk-6cd8454a1b2c3d4e5f60718293a4b5c6 e task-9f86d081884c7d659a2feaa0c55ad015",
            "a tarefa de risco task_{s}k-handler_para_os_usuarios_do_sistema passou",
            "my_{g}hp_helper_function_for_github_tokens and mailbox_{x}oxb-message-handler-for-slack",
            "o fiscal-{s}k-relatorio-mensal-de-vendas-da-loja-nova saiu",
            "skeleton, whisky, Alaska, a{s}k-me-anything-about-the-project, xoxo",
            "o disco tem 512GB livres e o desk-01 está ligado",
        ] {
            let line = raw.replace("{s}", "s").replace("{g}", "g").replace("{x}", "x");
            assert_eq!(patterns(&line), line);
        }
    }

    /// Spec 140 T10: o corte por teto vem depois da máscara, então a chave cortada no meio não sobra.
    #[test]
    fn the_cut_after_the_mask_never_leaves_a_piece() {
        let text = format!("{}{}{}", "x".repeat(1490), "ghp_", tail(30));
        let cut: String = patterns(&text).chars().take(1500).collect();
        assert!(!cut.contains("ghp_"), "{cut}");
    }

    #[test]
    fn the_bot_token_and_the_final_dot_of_a_sentence() {
        clean_of(&format!("4242424242:{}", tail(35)), "token do bot {} no log");
        let out = patterns(&format!("use {}{}.", "ghp_", tail(24)));
        assert_eq!(out, "use ***.");
    }

    #[test]
    fn jwt_and_bearer() {
        let jwt = format!("eyJ{}.{}.{}", tail(12), tail(20), tail(16));
        clean_of(&jwt, "cookie {} vencido");
        assert_eq!(patterns(&format!("Authorization: Bearer {}", tail(8))), "Authorization: ***");
        assert_eq!(patterns(&format!("curl -H \"x: bearer {}\" url", tail(20))), "curl -H \"x: bearer ***\" url");
        assert_eq!(patterns("the bearer of bad news and bearer authentication"), "the bearer of bad news and bearer authentication");
    }

    #[test]
    fn assignments_in_sensitive_names() {
        for line in [
            "DB_PASSWORD=hunter2hunter",
            "export API_KEY=abc123def",
            "password: hunter2hunter",
            "\"client_secret\": \"valor com espaco\"",
            "{\\\"token\\\":\\\"abc123def\\\"}",
            "STRIPE_KEY = abc123def",
            "https://x.example/cb?access_token=abc123def&next=1",
            "Server=db;User Id=sa;Password=abc123def;Encrypt=true",
            "senha=minhasenha",
            "dbPassword=abc123def",
            "Authorization: Basic dXNlcjpwYXNz",
        ] {
            let out = patterns(line);
            for leak in ["hunter2", "abc123def", "valor com", "minhasenha", "dXNlcjpwYXNz"] {
                assert!(!out.contains(leak), "vazou: {out}");
            }
            assert!(out.contains(MASK), "{out}");
        }
        assert_eq!(patterns("a=1;Password=abc123def;Encrypt=true"), "a=1;Password=***;Encrypt=true");
    }

    #[test]
    fn plain_settings_are_not_secrets() {
        for line in [
            "max_tokens=1000",
            "tokens: 1500",
            "token_count=12",
            "token: 5000",
            "api_key = env:NANTS_KEY",
            "password=${DB_PASSWORD}",
            "secret: null",
            "token == other",
            "primary_key=id",
            "PWD=/home/pessoa/projeto",
            "password: ***",
            "Falhou por causa da senha: incorreta.",
            "O login caiu por causa do token: expirou",
            "a chave de acesso e o secret: venceram!",
            "ação recusada pela senha: inválida",
            "It failed because of the token: expired",
            "the password: incorrect.",
        ] {
            assert_eq!(patterns(line), line);
        }
        // No início da linha, com sinal no valor, com aspas ou com `=`, é atribuição.
        for (line, kept) in [
            ("password: hunter2hunter", "password: ***"),
            ("  password: hunter", "  password: ***"),
            ("- password: hunter", "- password: ***"),
            ("a senha: abc123def", "a senha: ***"),
            ("a senha: \"abcdef\"", "a senha: \"***\""),
            ("a senha = incorreta", "a senha = ***"),
            ("{password: hunter}", "{password: ***}"),
            ("the password: correcthorsebatterystaple", "the password: ***"),
        ] {
            assert_eq!(patterns(line), kept);
        }
    }

    /// Texto hostil: cada `:` tem um valor que vai até o fim. Quadrático, este teste não terminaria.
    #[test]
    fn a_long_hostile_text_is_read_in_linear_time() {
        let text = "token:$".repeat(400_000);
        assert_eq!(patterns(&text), text);
    }

    /// Texto hostil: muitas aberturas de PEM, cada uma até a linha em branco, e nenhum fim.
    #[test]
    fn many_pem_openings_without_an_end_are_read_in_linear_time() {
        let text = format!("{PEM_OPEN}x\n\n").repeat(400_000);
        assert_eq!(patterns(&text), "***\n\n".repeat(400_000));
    }

    /// A busca pelo fim só para quando não há mais nenhum: um fim depois de um bloco fechado vale.
    #[test]
    fn an_end_after_a_closed_block_still_counts() {
        let block = format!("{PEM_OPEN}A{PEM_DASHES}\nx\n{PEM_CLOSE}A{PEM_DASHES}");
        assert_eq!(patterns(&format!("{block} meio {block} fim")), "*** meio *** fim");
        let open = format!("{PEM_OPEN}A{PEM_DASHES}\nx");
        assert_eq!(patterns(&format!("{block} meio {open}\n\nfim {open}\n\nresto")), "*** meio ***\n\nfim ***\n\nresto");
    }

    #[test]
    fn urls_keep_the_host_and_lose_the_credential() {
        assert_eq!(patterns("https://user:s3nha@git.example/repo.git"), "https://***@git.example/repo.git");
        assert_eq!(patterns("postgres://app@db:5432/x e http://192.168.0.10:3000/p1"), "postgres://***@db:5432/x e http://192.168.0.10:3000/p1");
        assert_eq!(patterns("veja https://site.example/a@b"), "veja https://site.example/a@b");
    }

    #[test]
    fn pem_blocks_leave_whole() {
        let open = format!("{PEM_OPEN}RSA PRIVATE KEY{PEM_DASHES}");
        let close = format!("{PEM_CLOSE}RSA PRIVATE KEY{PEM_DASHES}");
        let body = tail(40);
        assert_eq!(patterns(&format!("antes\n{open}\n{body}\n{close}\ndepois")), "antes\n***\ndepois");
        assert_eq!(patterns(&format!("{{\\\"key\\\":\\\"{open}\\n{body}\\n{close}\\n\\\"}}")), "{\\\"key\\\":\\\"***\\n\\\"}");
        assert_eq!(patterns(&format!("{open}\n{body}\n\ndepois")), "***\n\ndepois", "sem o fim, para na linha em branco");
        assert_eq!(patterns(&format!("{open}\n{body}")), "***");
    }

    #[test]
    fn high_entropy_strings() {
        clean_of(&tail(32), "valor {} no log");
        clean_of(&format!("{}+/{}==", tail(20), tail(20)), "base64 {} no log");
        clean_of(&"k3j9x2m7q1w8e5r4t6y0u2i9o3p7a5s8".to_uppercase(), "base32 {} no log");
        clean_of("k3j9x2m7q1w8e5r4t6y0u2i9o3p7a5s8", "base36 {} no log");
        clean_of("kqmw7xtrp3nbvzs9hdlf2cjgy5rtwq8m", "base36 com trechos de letras {} no log");
        clean_of("xKwmQplD7rTvnsYbf2Hzqw9LtgkPaM3n", "base62 com trechos de letras {} no log");
    }

    /// Gerador fixo (xorshift64*): as mesmas cadeias em toda rodada.
    struct Fixed(u64);

    impl Fixed {
        fn next(&mut self, below: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            let value = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33;
            usize::try_from(value).unwrap_or(0) % below
        }

        fn string(&mut self, alphabet: &[u8], len: usize) -> String {
            (0..len).map(|_| char::from(alphabet[self.next(alphabet.len())])).collect()
        }
    }

    /// Quantas de mil cadeias do alfabeto, nos tamanhos dados, saem mascaradas.
    fn masked_of_1000(alphabet: &[u8], sizes: std::ops::RangeInclusive<usize>) -> usize {
        let mut random = Fixed(0x9E37_79B9_7F4A_7C15);
        let sizes: Vec<usize> = sizes.collect();
        (0..1000)
            .filter(|n| {
                let value = random.string(alphabet, sizes[n % sizes.len()]);
                !patterns(&format!("valor {value} no log")).contains(&value)
            })
            .count()
    }

    #[test]
    fn a_thousand_random_strings_are_masked() {
        let base36: Vec<u8> = (b'a'..=b'z').chain(b'0'..=b'9').collect();
        let base62: Vec<u8> = (b'A'..=b'Z').chain(base36.iter().copied()).collect();
        for (name, alphabet) in [("base36", &base36), ("base62", &base62)] {
            let all = masked_of_1000(alphabet, 24..=64);
            let short = masked_of_1000(alphabet, 24..=26);
            println!("{name}: {all}/1000 de 24 a 64, {short}/1000 de 24 a 26");
            assert!(all >= 990, "{name}: {all}");
            assert!(short >= 980, "{name}, curtas: {short}");
        }
    }

    /// O Nanty fala destes ids o tempo todo: nenhum pode virar `***`.
    #[test]
    fn the_ids_nanty_talks_about_survive() {
        for line in [
            "commit 6cd8454a e 00d86fe7 na main",
            "sha 6cd8454a1b2c3d4e5f60718293a4b5c6d7e8f901 inteiro",
            "sha256 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "SHA 9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08",
            "uuid 123e4567-e89b-12d3-a456-426614174000 no log",
            "UUID 123E4567-E89B-12D3-A456-426614174000 no log",
            "tarefa T04 da spec 140-nanty-parceiro, cartão a1b2c3d4e5f60718",
            "spec 2026-11-01-relatorio-final-do-projeto-da-loja-nova",
            "em src/server/nanty/telegram/outbox.rs:1127 e /home/pessoa/Dev/Git/loja-nova/src/agents/llm_client.rs",
            "modelo gemini-3.8-flash-preview-2026-10-15 e gpt-6-luna",
            "getUserProfileByIdAndTenantName e TaintKindProjectFile",
            "teste ShouldReturn404WhenUserNotFound falhou",
            "base64EncodedSha256HashOfTheFile no log",
            "testOAuth2TokenRefreshWithHttp2Client e parseUtf8StringFromBase64Payload",
            "handleHttp500ErrorFromUpstreamServer e convertUtf16ToUtf8WithoutAllocation",
            "testIpv6AddressParsingWithPort8080 e Sha256DigestOfTheUploadedArchive",
            "shouldRetry3TimesAfterHttp503Response e decodeJwtTokenWithRs256Signature",
            "Http2ConnectionPoolTimeoutException e TestS3BucketPolicyForRegionUsEast1",
            "X509CertificateChainValidatorTest e OAuth2AuthorizationCodeGrantFlowTest",
            "test2FactorAuthenticationWithTotpCode e Ec2InstanceMetadataServiceV2Client",
            "Md5ChecksumMismatchOnFileUpload2 e whenUserHas2ActiveSessionsThenLogoutAll",
            "getS3ObjectUrlWithPresignedExpiry3600 e renderH264VideoFrameToWebGl2Canvas",
            "Vp9EncoderSettingsForMobile1080p e Grpc2xxResponseHandlerIntegrationTest",
            "shouldParseIso8601DateWithTimezoneOffset e Aes256GcmEncryptionKeyRotationTest",
            "HTTP2ServerConnectionPoolTimeout e testHTTPSClientWithTLS13Handshake",
            "XMLParser2ForSOAPEnvelopeHandling e ConvertToISO8859Encoding1252Fallback",
            "useEffect2ndRenderShouldNotRefetchUserProfile e fetchUserById2AndCacheInRedisCluster3",
            "base64encodedsha256hashofthefile e shouldreturn404whenusernotfound",
            "cartaoDeCredito3DSValidacaoDoPagamento e deveRetornar404QuandoUsuarioNaoExiste",
            "testeIntegracaoPagamento2Parcelas e api2v3CompatibilityLayerForLegacyClients",
            "XMLHTTPRequestHandlerForSOAPClient e parseJSONResponseFromRESTEndpoint",
            "AbstractSingletonProxyFactoryBean e HTTPSConnectionPoolManagerFactory",
            "ids: project_file, nanty.chat.silence_set, worktree-agent-acbd5592649e9e07a",
        ] {
            assert_eq!(patterns(line), line);
        }
    }

    /// Todo texto fixo do Nanty passa pelo `format::clean`: nenhuma frase pode perder um pedaço.
    #[test]
    fn no_fixed_sentence_of_nanty_changes() {
        let dictionaries = [crate::i18n::pt_br::MESSAGES, crate::i18n::en::MESSAGES];
        for (key, text) in dictionaries.iter().flat_map(|d| d.iter()).filter(|(key, _)| key.starts_with("nanty.")) {
            assert_eq!(patterns(text), *text, "{key}");
        }
    }

    #[test]
    fn the_known_secrets_and_the_patterns_go_together_and_it_is_stable() {
        let known = format!("segredo-{}", "z".repeat(12));
        let secrets = SecretSet::new([known.clone()]);
        let text = format!("{known} e {}{}", "ghp_", tail(24));
        let once = scrub(&secrets, &text);
        assert_eq!(once, "*** e ***");
        assert_eq!(scrub(&secrets, &once), once);
    }

    #[test]
    fn text_with_accents_is_safe_around_the_cuts() {
        assert_eq!(patterns("ação: não é senha, é coração"), "ação: não é senha, é coração");
        let out = patterns(&format!("senha=ação {}{} fim é", "ghp_", tail(24)));
        assert!(out.ends_with("*** fim é"), "{out}");
    }
}
