//! Spec 115 T01 (decisão 4, C-16): paridade cruzada entre os dicionários do servidor
//! (`src/i18n/*.rs`) e os da web (`web/src/i18n/ptBR.ts` e `en.ts`).
//!
//! - Todo `code` que o servidor emite (erros da API, mensagens do sistema, falhas do relatório,
//!   rótulos da atividade, teste de conexão e detalhes dos pré-requisitos) tem a chave da web
//!   correspondente nos dois idiomas.
//! - As chaves que precisam ter o mesmo texto no servidor e na web (o servidor cita o rótulo do
//!   botão, o detalhe do pré-requisito vira o texto de reserva) são iguais em cada idioma.
//! - Erros da API (spec 115 T02): todo código emitido por `ApiError::coded` tem o texto de reserva
//!   no dicionário do servidor, nos dois idiomas; nenhuma chave `serverError.*` sobra sem uso, nem no
//!   servidor nem na web; e a web não reconhece mais erro pelo texto (`KNOWN_SERVER_MESSAGES`,
//!   `KEY_BY_TEXT` e `serverMessage.*` saíram).
//!
//! Cada lista de exceções tem o motivo, e exceção sem uso falha.

#[path = "support/rust_tokens.rs"]
mod rust_tokens;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use nants::i18n::{en, lookup, pt_br, Lang};
use rust_tokens::{matching, production_sources, production_tokens, str_consts, Tok, Token};

// --- dicionários da web ---

/// Lê uma string JS (`"..."` ou `'...'`) a partir de `chars[at]`; devolve o texto e o índice
/// depois dela.
fn js_string(chars: &[char], at: usize) -> Option<(String, usize)> {
    let quote = *chars.get(at)?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut out = String::new();
    let mut k = at + 1;
    while k < chars.len() && chars[k] != quote {
        if chars[k] == '\\' {
            k += 1;
            match chars.get(k) {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('u') => {
                    let hex: String = chars.get(k + 1..k + 5)?.iter().collect();
                    out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)?);
                    k += 4;
                }
                Some(c) => out.push(*c),
                None => return None,
            }
        } else {
            out.push(chars[k]);
        }
        k += 1;
    }
    Some((out, k + 1))
}

fn skip_space(chars: &[char], mut k: usize) -> usize {
    while chars.get(k).is_some_and(|c| c.is_whitespace()) {
        k += 1;
    }
    k
}

/// Valor JS a partir de `at`: uma string ou strings unidas por `+`.
fn js_value(chars: &[char], at: usize) -> Option<(String, usize)> {
    let (mut text, mut k) = js_string(chars, skip_space(chars, at))?;
    loop {
        let next = skip_space(chars, k);
        if chars.get(next) != Some(&'+') {
            return Some((text, k));
        }
        let (more, end) = js_string(chars, skip_space(chars, next + 1))?;
        text.push_str(&more);
        k = end;
    }
}

/// Dicionário da web: as linhas `  "chave": "texto",` (o texto pode vir na linha de baixo).
fn web_dict(name: &str) -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/i18n").join(name);
    let source = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let chars: Vec<char> = source.chars().collect();
    let mut out = BTreeMap::new();
    let mut entries = 0;
    let mut k = 0;
    while k < chars.len() {
        let line_start = k == 0 || chars[k - 1] == '\n';
        if line_start && chars.get(k..k + 3) == Some(&[' ', ' ', '"']) {
            if let Some((key, after)) = js_string(&chars, k + 2) {
                if chars.get(after) == Some(&':') {
                    entries += 1;
                    let (value, end) = js_value(&chars, after + 1).unwrap_or_else(|| panic!("{name}: valor ilegível em {key}"));
                    out.insert(key, value);
                    k = end;
                    continue;
                }
            }
        }
        k += 1;
    }
    assert_eq!(out.len(), entries, "{name}: chave repetida");
    assert!(out.len() > 1000, "{name}: dicionário sem chaves ({})", out.len());
    out
}

// --- códigos emitidos pelo servidor ---

/// Código emitido: arquivo, linha, família da chave na web (`serverError`, `system`...) e código.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Emitted {
    family: &'static str,
    code: String,
    file: String,
    line: usize,
}

/// Constantes `CODE_*` por arquivo: a família da chave na web. Constante `CODE_*` num arquivo fora
/// desta tabela falha (quem cria a família decide a chave da web aqui).
const CODE_CONST_FAMILIES: &[(&str, &str)] = &[
    ("server/security.rs", "serverError"),
    ("server/app.rs", "serverError"),
    ("server/console.rs", "serverError"),
    ("server/app_guide.rs", "serverError"),
    ("server/devices.rs", "serverError"),
    ("server/chat.rs", "serverError"),
    ("server/runs.rs", "system"),
    ("server/approvals.rs", "system"),
    ("server/image_gen.rs", "system"),
    // Spec 133 T11: a análise de recuperação e o diagnóstico de ambiente na conversa.
    ("server/recovery.rs", "system"),
    ("server/report.rs", "report.failure"),
    // Spec 131 T06: as linhas do "o que mudou no plano" (`plan-changes.json`).
    ("sdd/plan_diff.rs", "planChange"),
];

/// Famílias de mensagem do sistema (`system.<família>.<nome>`): todo literal com essa forma no
/// código de produção é um código de aviso.
const SYSTEM_FAMILIES: [&str; 4] = ["run", "image", "task", "final"];

/// Funções que devolvem códigos de erro da API em literais (`SafeFsError::code`), por arquivo.
const CODE_FNS: &[(&str, &str)] = &[("server/safe_fs.rs", "code")];

/// Argumentos de nível 0 da chamada aberta em `open`: (início, fim exclusivo) de cada um.
fn call_args(toks: &[Token], open: usize) -> Vec<(usize, usize)> {
    let close = matching(toks, open);
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = open + 1;
    for k in open + 1..close.min(toks.len()) {
        match &toks[k].tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => depth -= 1,
            Tok::Punct(',') if depth == 0 => {
                out.push((start, k));
                start = k + 1;
            }
            _ => {}
        }
    }
    if start < close {
        out.push((start, close));
    }
    out
}

/// Tipos que viram corpo de erro da API.
const ERROR_TYPES: [&str; 3] = ["ApiError", "Rejection", "Response"];

/// Funções e closures do arquivo que montam um erro da API a partir de um parâmetro `code`: nome
/// e posição dele. As chamadas delas emitem o código passado nessa posição (`conflict(SPEC_DRAFT)`).
/// Função: devolve um dos [`ERROR_TYPES`]; closure: o corpo chama `ApiError::coded` logo de início.
fn code_helpers(toks: &[Token]) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let position = |params: &[(usize, usize)]| {
        params.iter().position(|&(a, b)| b > a + 1 && toks[a].is_ident("code") && toks[a + 1].is_punct(':'))
    };
    for i in 0..toks.len().saturating_sub(3) {
        let Some(name) = toks[i + 1].ident() else { continue };
        if toks[i].is_ident("fn") && toks[i + 2].is_punct('(') {
            let close = matching(toks, i + 2);
            let returns_error = toks.get(close + 1).is_some_and(|t| t.is_punct('-'))
                && toks.get(close + 2).is_some_and(|t| t.is_punct('>'))
                && toks.get(close + 3).and_then(Token::ident).is_some_and(|ty| ERROR_TYPES.contains(&ty));
            if let Some(k) = position(&call_args(toks, i + 2)).filter(|_| returns_error) {
                out.insert(name.to_string(), k);
            }
        }
        let closure_with_code = toks[i].is_ident("let")
            && toks[i + 2].is_punct('=')
            && toks.get(i + 3).is_some_and(|t| t.is_punct('|'))
            && toks.get(i + 4).is_some_and(|t| t.is_ident("code"))
            && toks.get(i + 5).is_some_and(|t| t.is_punct(':'));
        if closure_with_code && toks[i + 6..].iter().take(16).any(|t| t.is_ident("coded")) {
            out.insert(name.to_string(), 0);
        }
    }
    out
}

fn is_code_shape(text: &str) -> bool {
    !text.is_empty() && text.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

fn emitted_codes(sources: &[(String, String)]) -> Vec<Emitted> {
    let parsed: Vec<(&str, Vec<Token>)> = sources
        .iter()
        .filter(|(rel, _)| !rel.starts_with("i18n/"))
        .map(|(rel, text)| (rel.as_str(), production_tokens(text)))
        .collect();
    let mut global: HashMap<String, Vec<String>> = HashMap::new();
    for (_, toks) in &parsed {
        for (name, text) in str_consts(toks) {
            global.entry(name).or_default().push(text);
        }
    }
    let mut out = Vec::new();
    for (rel, toks) in &parsed {
        let local: HashMap<String, String> = str_consts(toks).into_iter().collect();
        let resolve = |t: &Token| -> Option<String> {
            match &t.tok {
                Tok::Str(s) => Some(s.clone()),
                Tok::Ident(name) => local.get(name).cloned().or_else(|| match global.get(name).map(Vec::as_slice) {
                    Some([only]) => Some(only.clone()),
                    _ => None,
                }),
                Tok::Punct(_) => None,
            }
        };
        let mut push = |family: &'static str, code: String, line: usize| {
            out.push(Emitted { family, code, file: (*rel).to_string(), line });
        };
        let helpers = code_helpers(toks);
        // Literais das funções de código da tabela `CODE_FNS`.
        for (file, func) in CODE_FNS {
            if file != rel {
                continue;
            }
            let at = toks.windows(2).position(|w| w[0].is_ident("fn") && w[1].is_ident(func)).unwrap_or_else(|| panic!("{file}: fn {func}"));
            let open = (at..toks.len()).find(|&k| toks[k].is_punct('{')).unwrap_or(toks.len());
            for t in &toks[open..matching(toks, open)] {
                if let Some(code) = t.str().filter(|c| is_code_shape(c)) {
                    push("serverError", code.to_string(), t.line);
                }
            }
        }
        for (name, text) in str_consts(toks) {
            if name.starts_with("CODE_") {
                let family = CODE_CONST_FAMILIES
                    .iter()
                    .find(|(f, _)| f == rel)
                    .map(|(_, family)| *family)
                    .unwrap_or_else(|| panic!("constante {name} em src/{rel}: diga a família da web em CODE_CONST_FAMILIES"));
                let line = toks.iter().find(|t| t.is_ident(&name)).map_or(0, |t| t.line);
                push(family, text, line);
            }
        }
        for (i, t) in toks.iter().enumerate() {
            let next = |k: usize| toks.get(i + k);
            match &t.tok {
                // `ApiError::coded(status, CODE, ..)` e as funções com parâmetro `code`: erro da API.
                Tok::Ident(name)
                    if next(1).is_some_and(|t| t.is_punct('('))
                        && !toks.get(i.wrapping_sub(1)).is_some_and(|t| t.is_ident("fn") || t.is_punct('.'))
                        && ((name == "coded" && toks.get(i.wrapping_sub(1)).is_some_and(|t| t.is_punct(':'))) || helpers.contains_key(name)) =>
                {
                    let position = if name == "coded" { 1 } else { helpers[name] };
                    let args = call_args(toks, i + 1);
                    if let Some(code) = args.get(position).filter(|(a, b)| b - a == 1).and_then(|(a, _)| resolve(&toks[*a])) {
                        if is_code_shape(&code) {
                            push("serverError", code, t.line);
                        }
                    }
                }
                // `json!({.., "code": X, ..})`: com ponto é aviso do sistema; sem, erro da API.
                Tok::Str(s) if s == "code" && next(1).is_some_and(|t| t.is_punct(':')) && !next(2).is_some_and(|t| t.is_punct(':')) => {
                    if let Some(code) = next(2).and_then(&resolve).filter(|c| is_code_shape(c)) {
                        push(if code.contains('.') { "system" } else { "serverError" }, code, t.line);
                    }
                }
                // `ActivityCode::new("tool.x", ..)` e o atalho `c("tool.x", ..)` de `agents/activity.rs`.
                Tok::Ident(name)
                    if (name == "new" && i >= 3 && toks[i - 3].is_ident("ActivityCode") || (name == "c" && *rel == "agents/activity.rs"))
                        && next(1).is_some_and(|t| t.is_punct('(')) =>
                {
                    if let Some(code) = next(2).and_then(Token::str) {
                        push("activity.code", code.to_string(), t.line);
                    }
                }
                // `Failure { code: "x", .. }` do teste de conexão.
                Tok::Ident(name) if name == "code" && *rel == "server/models_api.rs" && next(1).is_some_and(|t| t.is_punct(':')) && !next(2).is_some_and(|t| t.is_punct(':')) => {
                    if let Some(code) = next(2).and_then(Token::str) {
                        push("settings.test.error", code.to_string(), t.line);
                    }
                }
                Tok::Str(s) => {
                    // Detalhe do pré-requisito que vai à interface (`detail_code`, sem o prefixo).
                    if let Some(code) = s.strip_prefix("doctor.detail.").filter(|c| !c.is_empty() && *rel != "server/doctor.rs") {
                        push("prereq.detail", code.to_string(), t.line);
                    }
                    if is_code_shape(s) && s.split_once('.').is_some_and(|(f, rest)| SYSTEM_FAMILIES.contains(&f) && !rest.is_empty() && !rest.contains('.')) {
                        push("system", s.clone(), t.line);
                    }
                }
                _ => {}
            }
        }
    }
    out.sort();
    out.dedup_by(|a, b| a.family == b.family && a.code == b.code && a.file == b.file);
    out
}

/// Códigos sem a chave `<família>.<código>` na web, com o motivo.
const CODE_EXCEPTIONS: &[(&str, &str, &str)] = &[(
    "settings.test.error",
    "http_error",
    "o teste de conexão com HTTP vira `settings.test.httpStatus`/`settings.test.http` (com o status), não uma chave por código",
)];

#[test]
fn every_server_code_has_a_web_key_in_both_languages() {
    let pt = web_dict("ptBR.ts");
    let en = web_dict("en.ts");
    assert_eq!(pt.keys().collect::<Vec<_>>(), en.keys().collect::<Vec<_>>(), "ptBR.ts e en.ts com chaves diferentes");
    let codes = emitted_codes(&production_sources());
    for family in ["serverError", "system", "report.failure", "activity.code", "settings.test.error", "prereq.detail", "planChange"] {
        assert!(codes.iter().any(|c| c.family == family), "nenhum código da família {family}: a varredura se perdeu");
    }
    let mut used = BTreeSet::new();
    let mut missing = Vec::new();
    for c in &codes {
        let key = format!("{}.{}", c.family, c.code);
        if pt.contains_key(&key) && en.contains_key(&key) {
            continue;
        }
        // Código com `{count}` (spec 137 T12): as formas `.one` e `.other`, nos dois idiomas.
        let plural = [format!("{key}.one"), format!("{key}.other")];
        if plural.iter().all(|k| pt.contains_key(k) && en.contains_key(k)) {
            continue;
        }
        match CODE_EXCEPTIONS.iter().position(|(f, code, _)| *f == c.family && *code == c.code) {
            Some(k) => {
                used.insert(k);
            }
            None => missing.push(format!("src/{}:{}: {key}", c.file, c.line)),
        }
    }
    assert!(missing.is_empty(), "código do servidor sem chave na web (nos dois idiomas):\n{}", missing.join("\n"));
    for (k, (family, code, why)) in CODE_EXCEPTIONS.iter().enumerate() {
        assert!(why.len() > 20, "{family}.{code}: motivo curto");
        assert!(used.contains(&k), "exceção sem uso: {family}.{code}");
    }
}

// --- mesmo texto no servidor e na web ---

/// Chaves com o mesmo texto no servidor e na web, por idioma: (chave do servidor, chave da web).
const SAME_TEXT: &[(&str, &str, &str)] = &[
    ("ui.app_start", "app.start", "o servidor cita ao modelo o rótulo do botão de iniciar o app, que precisa ser o da tela"),
    (
        "ui.app_open_console",
        "app.openConsole",
        "sem run_command, o servidor cita ao modelo o rótulo do botão que abre o Console, que precisa ser o da tela",
    ),
];

/// Famílias com o mesmo texto: (prefixo do servidor, prefixo da web, motivo). Toda chave do
/// servidor com par na web entra; a família precisa ter ao menos um par.
const SAME_TEXT_FAMILIES: &[(&str, &str, &str)] = &[
    (
        "doctor.detail.",
        "prereq.detail.",
        "o detalhe do pré-requisito vai à interface pelo código; o texto do servidor é a reserva e o do nants doctor",
    ),
    ("doctor.label.", "prereq.machine.", "nome do item do diagnóstico, o mesmo no nants doctor e na tela de Primeiros passos"),
    ("doctor.fix.", "prereq.fix.", "ação sugerida do item do diagnóstico, a mesma no nants doctor e na tela de Primeiros passos"),
    (
        "provider.kind.",
        "provider.kind.",
        "rótulo do motivo da falha do provedor (spec 119): o servidor o usa no texto de reserva e lê de volta; a tela o monta pelo `kind`",
    ),
    (
        "system.",
        "system.",
        "texto de reserva da mensagem do sistema com code (spec 115 T03): o servidor grava o mesmo texto que a web monta pelo código",
    ),
    (
        "plan_review.change.",
        "planChange.",
        "linha do resumo do que mudou no plano (spec 131 T06): o servidor a escreve ao Revisor no idioma do projeto e a web a monta pelo mesmo código",
    ),
    (
        "runner.marker.",
        "marker.",
        "marcador estrutural da mensagem do Revisor (spec 129 T04): o servidor o escreve no idioma do projeto e a web o lê em qualquer idioma pelas mesmas formas",
    ),
];

/// Pares conhecidos com texto diferente (e o motivo); a exceção falha quando os textos se igualam.
/// A spec 115 T04 acertou as quatro do acesso pela rede: a lista fica vazia.
const SAME_TEXT_DIVERGENCES: &[(&str, &str)] = &[];

#[test]
fn shared_texts_match_per_language() {
    let web = [(Lang::PtBr, web_dict("ptBR.ts")), (Lang::En, web_dict("en.ts"))];
    let mut pairs: Vec<(String, String)> = SAME_TEXT.iter().map(|(s, w, _)| ((*s).to_string(), (*w).to_string())).collect();
    for (server_prefix, web_prefix, why) in SAME_TEXT_FAMILIES {
        assert!(why.len() > 20, "{server_prefix}: motivo curto");
        let before = pairs.len();
        for (key, _) in pt_br::MESSAGES {
            if let Some(rest) = key.strip_prefix(server_prefix) {
                let web_key = format!("{web_prefix}{rest}");
                if web[0].1.contains_key(&web_key) {
                    pairs.push(((*key).to_string(), web_key));
                }
            }
        }
        assert!(pairs.len() > before, "família sem pares: {server_prefix} / {web_prefix}");
    }
    let mut diverged = BTreeSet::new();
    let mut problems = Vec::new();
    for (server_key, web_key) in &pairs {
        for (lang, dict) in &web {
            let server = lookup(*lang, server_key).unwrap_or_else(|| panic!("{server_key} sem texto no servidor"));
            let web_text = dict.get(web_key).unwrap_or_else(|| panic!("{web_key} sem texto na web"));
            if server == web_text {
                continue;
            }
            if SAME_TEXT_DIVERGENCES.iter().any(|(k, _)| k == server_key) {
                diverged.insert(server_key.clone());
                continue;
            }
            problems.push(format!("{lang:?}: {server_key} = {server:?}\n      {web_key} = {web_text:?}"));
        }
    }
    assert!(problems.is_empty(), "textos do servidor e da web diferentes:\n{}", problems.join("\n"));
    for (key, why) in SAME_TEXT_DIVERGENCES {
        assert!(why.len() > 20, "{key}: motivo curto");
        assert!(diverged.contains(*key), "{key} já tem o mesmo texto: tire de SAME_TEXT_DIVERGENCES");
    }
    // Os dois dicionários do servidor também têm as chaves (sem `unwrap` silencioso acima).
    assert_eq!(pt_br::MESSAGES.len(), en::MESSAGES.len());
}

// --- erros da API: reserva no servidor, nenhuma chave sobrando, nada por texto ---

/// Códigos `serverError` emitidos fora do código de produção do servidor, com o motivo.
const WEB_ONLY_SERVER_ERRORS: &[(&str, &str)] = &[];

#[test]
fn every_api_error_code_has_the_server_fallback_and_no_key_is_left_over() {
    let codes: BTreeSet<String> =
        emitted_codes(&production_sources()).into_iter().filter(|c| c.family == "serverError").map(|c| c.code).collect();
    assert!(codes.len() > 150, "códigos de erro da API: {}", codes.len());
    let mut missing = Vec::new();
    for code in &codes {
        let key = format!("serverError.{code}");
        for lang in [Lang::PtBr, Lang::En] {
            if lookup(lang, &key).is_none() {
                missing.push(format!("{lang:?}: {key}"));
            }
        }
    }
    assert!(missing.is_empty(), "código de erro sem texto de reserva no servidor:\n{}", missing.join("\n"));
    let unused: Vec<&str> = pt_br::MESSAGES
        .iter()
        .filter_map(|(k, _)| k.strip_prefix("serverError."))
        .filter(|code| !codes.contains(*code))
        .collect();
    assert!(unused.is_empty(), "chave serverError.* do servidor sem uso (apague):\n{}", unused.join("\n"));
}

#[test]
fn every_web_server_error_key_is_emitted_by_the_server() {
    let codes: BTreeSet<String> =
        emitted_codes(&production_sources()).into_iter().filter(|c| c.family == "serverError").map(|c| c.code).collect();
    let mut used = BTreeSet::new();
    let mut unused = Vec::new();
    for key in web_dict("ptBR.ts").keys() {
        let Some(code) = key.strip_prefix("serverError.") else { continue };
        if codes.contains(code) {
            continue;
        }
        match WEB_ONLY_SERVER_ERRORS.iter().position(|(c, _)| *c == code) {
            Some(k) => {
                used.insert(k);
            }
            None => unused.push(key.clone()),
        }
    }
    assert!(unused.is_empty(), "chave de erro da web sem código no servidor (apague):\n{}", unused.join("\n"));
    for (k, (code, why)) in WEB_ONLY_SERVER_ERRORS.iter().enumerate() {
        assert!(why.len() > 20, "{code}: motivo curto");
        assert!(used.contains(&k), "exceção sem uso: {code}");
    }
}

/// A web traduz erro só pelo código: o casamento por texto saiu (spec 115 T02, C-01 e C-17).
#[test]
fn the_web_no_longer_matches_server_errors_by_text() {
    let client = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/api/client.ts")).expect("client.ts");
    for gone in ["KNOWN_SERVER_MESSAGES", "KEY_BY_TEXT"] {
        assert!(!client.contains(gone), "client.ts ainda tem {gone}");
    }
    for name in ["ptBR.ts", "en.ts"] {
        let left: Vec<String> = web_dict(name).into_keys().filter(|k| k.starts_with("serverMessage.")).collect();
        assert!(left.is_empty(), "{name}: chaves serverMessage.* sobrando: {left:?}");
    }
}

#[test]
fn helpers_read_the_web_and_match_server_literals() {
    let chars: Vec<char> = "  \"a.b\":\n    \"linha \\\"um\\\" \" +\n    'dois',".chars().collect();
    assert_eq!(js_string(&chars, 2), Some(("a.b".to_string(), 7)));
    assert_eq!(js_value(&chars, 8).map(|(t, _)| t), Some("linha \"um\" dois".to_string()));
    assert!(is_code_shape("run.stopped") && !is_code_shape("Run") && !is_code_shape("a b"));
    let toks = production_tokens(
        "fn conflict(code: &'static str) -> Rejection { x }\n\
         fn note(code: &'static str) -> String { y }\n\
         fn f() { let api = |code: &'static str| Wrap(ApiError::coded(S, code, m)); conflict(\"a_b\"); }",
    );
    let helpers = code_helpers(&toks);
    assert_eq!(helpers.get("conflict"), Some(&0));
    assert_eq!(helpers.get("api"), Some(&0));
    assert!(!helpers.contains_key("note"), "função que não devolve erro da API não conta");
    let open = toks.iter().position(|t| t.is_ident("coded")).map_or(0, |k| k + 1);
    assert_eq!(call_args(&toks, open).len(), 3);
}

// --- títulos canônicos do spec.md e das tarefas na web (spec 131 T06) ---

/// Títulos que o servidor grava ou procura no `spec.md` e nas tarefas, sempre em pt-BR (a gravação
/// no idioma do projeto ficou para uma spec futura): (texto do servidor, chave da web). A web os
/// reconhece por `sddHeading.*` para mostrar no idioma ativo, então o texto pt-BR da chave precisa
/// ser o mesmo do servidor.
fn server_headings() -> Vec<(String, &'static str)> {
    use nants::agents::{finding_reply, finding_track, runner};
    use nants::sdd::{role_guidance, spec_sections};
    let fixed: [(&str, &'static str); 20] = [
        (runner::CHECKED_WARNINGS, "sddHeading.checkedWarnings"),
        (runner::OBJECTIVE, "sddHeading.objective"),
        (runner::ACCEPTANCE, "sddHeading.criteria"),
        (runner::FINDINGS, "sddHeading.findings"),
        (runner::FINDINGS_LOG, "sddHeading.findingsLog"),
        (runner::GUIDANCE_SECTION, "sddHeading.humanGuidance"),
        (runner::FLAKY_SECTION, "sddHeading.flaky"),
        (runner::REUSE_SECTION, "sddHeading.reuse"),
        (runner::FORMAT_SECTION, "sddHeading.format"),
        (runner::INSTALL_SECTION, "sddHeading.install"),
        (runner::INSTALL_FOOTPRINT_SECTION, "sddHeading.installFootprint"),
        (runner::ADVICE_SECTION, "sddHeading.advice"),
        (runner::GLOBAL_CRITERIA, "sddHeading.globalCriteria"),
        (runner::HISTORY, "sddHeading.history"),
        (finding_track::REGISTRY, "sddHeading.registry"),
        (finding_reply::RESPONSES, "sddHeading.responses"),
        (spec_sections::PREMISES, "sddHeading.premises"),
        (role_guidance::ROLE_GUIDANCE_SECTION, "sddHeading.roleGuidance"),
        (role_guidance::ROLE_DEVELOPER, "sddHeading.roleDeveloper"),
        (role_guidance::ROLE_REVIEWER, "sddHeading.roleReviewer"),
    ];
    let mut out: Vec<(String, &'static str)> = fixed.iter().map(|(text, key)| ((*text).to_string(), *key)).collect();
    out.push((format!("{}{{n}}", runner::RECOVERY_PREFIX), "sddHeading.numbered.recovery"));
    out
}

/// Títulos do exemplo de spec que o Arquiteto recebe (não há constante no servidor): o exemplo
/// precisa continuar usando o mesmo texto da web.
const EXAMPLE_HEADINGS: [&str; 2] = ["sddHeading.context", "sddHeading.approach"];

#[test]
fn canonical_sdd_headings_on_the_web_match_the_server() {
    let pt = web_dict("ptBR.ts");
    let en = web_dict("en.ts");
    for (text, key) in server_headings() {
        assert_eq!(pt.get(key).map(String::as_str), Some(text.as_str()), "{key}: o pt-BR da web difere do título do servidor");
        let english = en.get(key).unwrap_or_else(|| panic!("{key} sem en"));
        assert_ne!(english, &text, "{key}: sem tradução em en");
    }
    let example = nants::agents::prompts::EXAMPLE_SPEC_MD;
    for key in EXAMPLE_HEADINGS {
        let text = pt.get(key).unwrap_or_else(|| panic!("{key} sem pt"));
        assert!(example.contains(&format!("## {text}\n")), "{key}: o exemplo do Arquiteto não usa mais {text:?}");
    }
    // Toda chave `sddHeading.*` da web tem os dois idiomas e está na tabela de `sddHeadings.ts`.
    let source = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/i18n/sddHeadings.ts")).expect("sddHeadings.ts");
    for key in pt.keys().filter(|k| k.starts_with("sddHeading.")) {
        assert!(en.contains_key(key), "{key} sem en");
        assert!(source.contains(&format!("\"{key}\"")), "{key} fora da tabela de sddHeadings.ts");
    }
}
