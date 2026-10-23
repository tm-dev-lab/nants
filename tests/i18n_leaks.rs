//! Spec 115 T01 (decisão 4, C-15.1): varredura do código Rust contra texto de pessoa fora dos
//! dicionários (`src/i18n/pt_br.rs` e `src/i18n/en.rs`).
//!
//! Lê os tokens de produção de `src/**/*.rs` (sem comentários, sem `#[cfg(test)]`/`mod tests` e
//! sem os arquivos `*_tests.rs`) e olha os literais que caem nos caminhos que chegam à pessoa:
//! - `ApiError::*(..)`, `Rejection::*(..)`, `json!({"error": ..})`, `json_response(..)` e as
//!   funções e closures do arquivo que recebem texto e devolvem um desses erros: erro da API;
//! - `AgentMessage { text: .. }` e `SystemNotice { text: .. }`: mensagens da conversa;
//! - `say(..)`, `tests_message(..)`, `final_notice(..)` e `deliver(..)`: mensagens da conversa e
//!   do diálogo;
//! - `json!({"text": ..})`: mensagem montada direto em JSON;
//! - `next_action = ..` / `next_action: ..`: o motivo gravado na tarefa e mostrado no quadro;
//! - `println!`, `eprintln!`, `print!` e `eprint!`: mensagens do terminal (CLI e doctor).
//!
//! Um literal com texto de pessoa (letra do português, palavra do pt sem acento ou frase de duas
//! palavras ou mais, em pt ou en) nesses caminhos precisa estar na lista de exceções, com o
//! motivo. Constantes de texto (`const X: &str = ".."`) usadas nesses caminhos contam como o
//! literal delas. Exceção sem uso falha: a lista só encolhe.

#[path = "support/rust_tokens.rs"]
mod rust_tokens;

use std::collections::{BTreeSet, HashMap};

use rust_tokens::{matching, production_sources, production_tokens, str_consts, Tok, Token};

/// Arquivos fora da varredura, com o motivo.
const SKIPPED_FILES: &[(&str, &str)] = &[
    ("i18n/pt_br.rs", "dicionário pt-BR: é a fonte dos textos em português"),
    ("i18n/en.rs", "dicionário en: é a fonte dos textos em inglês"),
    (
        "agents/prompts.rs",
        "prompts dos modelos: instruções para o modelo, em português por decisão da spec 102 (decisão 3), nunca exibidas à pessoa",
    ),
    (
        "sdd/role_guidance.rs",
        "orientação dos papéis lida pelos modelos (prompts), em português por decisão da spec 102, nunca exibida à pessoa",
    ),
];

/// Letras do português que o inglês não usa (as mesmas da web, `ptLeftovers.ts`).
const PT_LETTERS: &str = "ãõçáéíóúâêôàÃÕÇÁÉÍÓÚÂÊÔÀ";

/// Palavras comuns do português escritas sem acento (as da web, `ptLeftovers.ts`, e as do
/// sentinela de `i18n::tests`); nenhuma é palavra do inglês.
const PT_UNACCENTED: &[&str] = &[
    "nao", "voce", "voces", "tambem", "entao", "execucao", "configuracao", "conexao", "sessao", "opcoes", "acoes", "pendencia",
    "pendencias", "informacao", "descricao", "atencao", "criterios", "testes", "checagem", "checagens", "esgotado", "arquivos",
    "falhou", "arvore", "pergunta", "resposta", "levantamento", "iniciar", "tarefa", "tarefas", "projeto", "pasta",
];

/// Caminho de pessoa onde o literal foi achado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    ApiError,
    ConversationText,
    Say,
    FinalNotice,
    Deliver,
    JsonText,
    NextAction,
    Terminal,
    /// Spec 119: corpo das funções que montam o motivo de uma falha do provedor.
    ProviderReason,
}

/// Spec 119: funções (arquivo, nome) que montam o motivo de uma falha do provedor, o detalhe entre
/// parênteses que chega à conversa, ao cartão de pausa, ao Quadro e à Atividade. O rótulo sai do
/// dicionário (`provider.kind.*`); nenhum literal com texto de pessoa no corpo delas.
const PROVIDER_REASON_FNS: &[(&str, &str)] = &[
    ("agents/llm_client/bad_reason.rs", "label"),
    ("agents/llm_client/bad_reason.rs", "detail_in"),
    ("agents/llm_client/bad_reason.rs", "detail"),
    ("agents/llm_client/bad_reason.rs", "parse_detail"),
    ("agents/llm_client.rs", "detail_in"),
    ("agents/llm_client.rs", "detail"),
    ("agents/llm_client.rs", "describe"),
    ("agents/llm_client.rs", "reason_kind"),
    ("agents/llm_client.rs", "throttle_label"),
    ("agents/llm_client.rs", "retry_notice"),
    ("agents/llm_client.rs", "described_params"),
    ("server/error.rs", "provider_failure_params"),
    ("server/chat.rs", "architect_error"),
];

/// Regiões (corpo) das funções de [`PROVIDER_REASON_FNS`] em `file`.
fn provider_reason_regions(file: &str, toks: &[Token]) -> Vec<(usize, usize, Kind)> {
    let mut out = Vec::new();
    for (_, name) in PROVIDER_REASON_FNS.iter().filter(|(f, _)| *f == file) {
        for i in 0..toks.len() {
            if !(toks[i].is_ident("fn") && toks.get(i + 1).is_some_and(|t| t.is_ident(name))) {
                continue;
            }
            let Some(open) = toks[i..].iter().position(|t| t.is_punct('{')).map(|p| i + p) else { continue };
            out.push((open + 1, matching(toks, open), Kind::ProviderReason));
        }
    }
    out
}

/// Literal de texto de pessoa num caminho de pessoa.
#[derive(Debug, Clone)]
struct Hit {
    file: String,
    line: usize,
    kind: Kind,
    text: String,
}

/// Exceção: no `file`, no caminho `kind`, um literal que contém `snippet`.
struct Exception {
    file: &'static str,
    kind: Kind,
    snippet: &'static str,
    why: &'static str,
}

/// Linha da mensagem do Revisor que a web lê (`reviewMessage.ts`, `FINAL_COUNTS`) e mostra no idioma
/// ativo pelo dicionário (`review.finalPrefix`).
const REVIEW_MARKER: &str = "marcador estrutural da mensagem do Revisor, lido pela web (`reviewMessage.ts`) e mostrado no idioma ativo pelo dicionário; igual em todos os idiomas (spec 115 T03)";
/// `next_action` de tarefa fora de BLOCKED: a web só mostra o de tarefa BLOCKED (quadro e card).
const INTERNAL_NOTE: &str = "nota interna da tarefa fora de BLOCKED: a web só mostra o next_action de tarefa BLOCKED, e o bloqueio grava o motivo no idioma do projeto (spec 114 T02 e 115 T03)";
/// Log técnico do servidor no terminal: falha interna de limpeza, gravação, varredura ou provedor.
/// A pessoa recebe o efeito pela interface, no idioma dela; as mensagens do terminal que ela lê
/// (dispositivos, link de entrada, avisos de modo, ambientes órfãos) estão no dicionário (spec 115 T04).
const LOG: &str = "log técnico de depuração no terminal (falha interna de limpeza, gravação, varredura ou provedor), para diagnóstico de quem opera o servidor; a pessoa recebe o efeito pela interface no idioma dela, e as mensagens do terminal que ela lê estão no dicionário (spec 115 T04)";
/// Linha fixa que os testes e scripts leem para achar o endereço do servidor.
const MAIN: &str = "linha estrutural lida pelos testes e scripts (`tests/smoke.rs`, `tests/launcher.rs`); igual em todos os idiomas (spec 102)";

const fn ex(file: &'static str, kind: Kind, snippet: &'static str, why: &'static str) -> Exception {
    Exception { file, kind, snippet, why }
}

/// Lista inicial (spec 115 T01): a T02 eliminou as de `ApiError` (todas com código), a T03 as das
/// mensagens da conversa e a T04 as mensagens do terminal que a pessoa lê. Ficam só as legítimas:
/// marcadores estruturais lidos por parser (`REVIEW_MARKER`, `MAIN`), a nota interna da tarefa fora
/// de BLOCKED (`INTERNAL_NOTE`) e os logs técnicos de depuração (`LOG`).
const EXCEPTIONS: &[Exception] = &[
    ex("agents/architect.rs", Kind::Terminal, "nants: checagem do plano: continuação", LOG),
    ex("agents/llm_client.rs", Kind::Terminal, "nants: falha do provedor: papel={role}", LOG),
    ex(
        "agents/llm_client.rs",
        Kind::ProviderReason,
        "HTTP 429",
        "rótulo técnico do status HTTP no aviso de limite, igual em todos os idiomas; a frase em volta vem do dicionário pelo code `pace.throttled` (spec 119)",
    ),
    ex("agents/llm_client/pace.rs", Kind::Terminal, "nants: ritmo conexão={} modelo={}", LOG),
    ex("agents/runner.rs", Kind::NextAction, "aguardando developer", INTERNAL_NOTE),
    ex("agents/runner.rs", Kind::Say, "Verificação final: {met} de {total}", REVIEW_MARKER),
    ex("main.rs", Kind::Terminal, "nants listening on http://{addr}", MAIN),
    ex("server/app.rs", Kind::Terminal, "aviso: down do -run de {project_id}", LOG),
    ex("server/architecture.rs", Kind::Terminal, "aviso: ARCHITECTURE.md não atualizado", LOG),
    ex("server/architecture.rs", Kind::Terminal, "aviso: lições do perfil {role}", LOG),
    ex("server/architecture.rs", Kind::Terminal, "aviso: lições dos perfis não", LOG),
    ex("server/envs.rs", Kind::Terminal, "aviso: containers com label", LOG),
    ex("server/envs.rs", Kind::Terminal, "aviso: docker rm falhou para {project}", LOG),
    ex("server/envs.rs", Kind::Terminal, "aviso: down de {project} falhou: {e}", LOG),
    ex("server/envs.rs", Kind::Terminal, "aviso: não foi possível derrubar o", LOG),
    ex("server/envs.rs", Kind::Terminal, "aviso: remoção de containers de", LOG),
    ex("server/envs.rs", Kind::Terminal, "aviso: varredura de ambientes Docker", LOG),
    ex("server/projects.rs", Kind::Terminal, "aviso: cleanup Docker de {id} não", LOG),
    ex("server/projects.rs", Kind::Terminal, "aviso: cleanup Docker de {id}: {e}", LOG),
    ex("server/report.rs", Kind::Terminal, "aviso: cleanup do ambiente de testes de", LOG),
    ex("server/runs.rs", Kind::Terminal, "execução interrompida (projeto {}, spec", LOG),
    ex("server/ui_state.rs", Kind::Terminal, "aviso: estado da interface do projeto", LOG),
];

/// Texto sem os marcadores do `format!` (`{}`, `{x}`, `{x:?}`) e sem URLs.
fn without_placeholders_and_urls(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find(['}', '{', ' ', '\n']) {
            Some(close) if after[close..].starts_with('}') => {
                out.push(' ');
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().filter(|w| !w.contains("://")).collect::<Vec<_>>().join(" ")
}

/// O literal é texto de pessoa: letra do português, palavra do pt sem acento ou duas palavras
/// naturais ou mais (pt ou en). Identificadores, chaves, caminhos e comandos soltos não contam.
fn is_person_text(text: &str) -> bool {
    let clean = without_placeholders_and_urls(text);
    if clean.chars().any(|c| PT_LETTERS.contains(c)) {
        return true;
    }
    let words: Vec<String> = clean
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| ".,;:!?()[]\"'`<>".contains(c)).to_lowercase())
        .filter(|w| w.chars().count() >= 2 && w.chars().all(char::is_alphabetic))
        .collect();
    if words.iter().any(|w| PT_UNACCENTED.contains(&w.as_str())) || words.len() >= 2 {
        return true;
    }
    // Rótulo de uma palavra só ("Aviso: {e}", "falha: {e}"); um código solto ("shutdown") não conta.
    let trimmed = clean.trim();
    words.len() == 1 && (trimmed.contains(": ") || trimmed.ends_with(':') || trimmed.chars().next().is_some_and(char::is_uppercase))
}

/// Literais e constantes de texto de `toks[from..to]`.
fn texts_in(toks: &[Token], from: usize, to: usize, consts: &dyn Fn(&str) -> Option<String>) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    for (k, t) in toks.iter().enumerate().take(to.min(toks.len())).skip(from) {
        match &t.tok {
            Tok::Str(s) => out.push((k, t.line, s.clone())),
            Tok::Ident(name) if name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') => {
                if let Some(text) = consts(name) {
                    out.push((k, t.line, text));
                }
            }
            _ => {}
        }
    }
    out
}

/// Fim do valor que começa em `from`: a `,` ou o `;` no mesmo nível, ou o fechamento do grupo.
fn value_end(toks: &[Token], from: usize) -> usize {
    let mut depth = 0i32;
    for (k, t) in toks.iter().enumerate().skip(from) {
        match &t.tok {
            Tok::Punct('(' | '[' | '{') => depth += 1,
            Tok::Punct(')' | ']' | '}') => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
            }
            Tok::Punct(',' | ';') if depth == 0 => return k,
            _ => {}
        }
    }
    toks.len()
}

/// `:` de campo (não `::`).
fn is_field_colon(toks: &[Token], k: usize) -> bool {
    toks.get(k).is_some_and(|t| t.is_punct(':'))
        && !toks.get(k + 1).is_some_and(|t| t.is_punct(':'))
        && !toks.get(k.wrapping_sub(1)).is_some_and(|t| t.is_punct(':'))
}

/// Regiões (início, fim, caminho) dos caminhos de pessoa no arquivo.
/// Tipos que viram corpo de erro da API.
const ERROR_TYPES: [&str; 2] = ["ApiError", "Rejection"];

/// Funções e closures do arquivo que recebem texto e devolvem um erro da API (`fn conflict(message:
/// &str) -> Rejection`, `let forbidden = |msg: &str| ApiError::new(..)`): as chamadas delas também
/// são caminho de erro. `json_response` vale em todo o projeto.
fn error_helpers(toks: &[Token]) -> BTreeSet<String> {
    let mut out = BTreeSet::from(["json_response".to_string()]);
    let takes_text = |params: &[Token]| params.iter().any(|t| t.is_ident("str") || t.is_ident("String"));
    for i in 0..toks.len() {
        let Some(name) = toks.get(i + 1).and_then(Token::ident) else { continue };
        if toks[i].is_ident("fn") && toks.get(i + 2).is_some_and(|t| t.is_punct('(')) {
            let close = matching(toks, i + 2);
            let returns_error = toks.get(close + 1).is_some_and(|t| t.is_punct('-'))
                && toks.get(close + 2).is_some_and(|t| t.is_punct('>'))
                && toks.get(close + 3).and_then(Token::ident).is_some_and(|ty| ERROR_TYPES.contains(&ty));
            if returns_error && takes_text(&toks[i + 3..close]) {
                out.insert(name.to_string());
            }
        }
        if toks[i].is_ident("let") && toks.get(i + 2).is_some_and(|t| t.is_punct('=')) && toks.get(i + 3).is_some_and(|t| t.is_punct('|')) {
            let Some(bar) = toks[i + 4..].iter().position(|t| t.is_punct('|')).map(|p| i + 4 + p) else { continue };
            let body_is_error = toks.get(bar + 1).and_then(Token::ident).is_some_and(|ty| ERROR_TYPES.contains(&ty));
            if body_is_error && takes_text(&toks[i + 4..bar]) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// Regiões (início, fim, caminho) dos caminhos de pessoa no arquivo.
fn person_regions(toks: &[Token]) -> Vec<(usize, usize, Kind)> {
    let mut out = Vec::new();
    let helpers = error_helpers(toks);
    let at = |k: usize| toks.get(k);
    for i in 0..toks.len() {
        let Some(name) = toks[i].ident() else { continue };
        let after_fn = i > 0 && toks[i - 1].is_ident("fn");
        match name {
            _ if ERROR_TYPES.contains(&name)
                && at(i + 1).is_some_and(|t| t.is_punct(':'))
                && at(i + 2).is_some_and(|t| t.is_punct(':'))
                && at(i + 3).and_then(Token::ident).is_some()
                && at(i + 4).is_some_and(|t| t.is_punct('(')) =>
            {
                out.push((i + 5, matching(toks, i + 4), Kind::ApiError));
            }
            _ if helpers.contains(name) && !after_fn && at(i + 1).is_some_and(|t| t.is_punct('(')) && !(i > 0 && toks[i - 1].is_punct('.')) => {
                out.push((i + 2, matching(toks, i + 1), Kind::ApiError));
            }
            "AgentMessage" | "SystemNotice" if at(i + 1).is_some_and(|t| t.is_punct('{')) => {
                let end = matching(toks, i + 1);
                let mut depth = 0i32;
                for k in i + 2..end {
                    match &toks[k].tok {
                        Tok::Punct('(' | '[' | '{') => depth += 1,
                        Tok::Punct(')' | ']' | '}') => depth -= 1,
                        Tok::Ident(f) if depth == 0 && f == "text" && is_field_colon(toks, k + 1) => {
                            out.push((k + 2, value_end(toks, k + 2), Kind::ConversationText));
                        }
                        _ => {}
                    }
                }
            }
            "say" | "tests_message" | "final_notice" | "deliver" if !after_fn && at(i + 1).is_some_and(|t| t.is_punct('(')) => {
                let kind = match name {
                    "say" | "tests_message" => Kind::Say,
                    "final_notice" => Kind::FinalNotice,
                    _ => Kind::Deliver,
                };
                out.push((i + 2, matching(toks, i + 1), kind));
            }
            "json" if at(i + 1).is_some_and(|t| t.is_punct('!')) && at(i + 2).is_some_and(|t| t.is_punct('(') || t.is_punct('{')) => {
                let end = matching(toks, i + 2);
                for k in i + 3..end {
                    let kind = match toks[k].str() {
                        Some("text") => Kind::JsonText,
                        Some("error") => Kind::ApiError,
                        _ => continue,
                    };
                    if is_field_colon(toks, k + 1) {
                        out.push((k + 2, value_end(toks, k + 2), kind));
                    }
                }
            }
            "next_action" if !after_fn => {
                let assign = at(i + 1).is_some_and(|t| t.is_punct('=')) && !at(i + 2).is_some_and(|t| t.is_punct('='));
                if assign || is_field_colon(toks, i + 1) {
                    out.push((i + 2, value_end(toks, i + 2), Kind::NextAction));
                }
            }
            "println" | "eprintln" | "print" | "eprint"
                if at(i + 1).is_some_and(|t| t.is_punct('!')) && at(i + 2).is_some_and(|t| t.is_punct('(')) =>
            {
                out.push((i + 3, matching(toks, i + 2), Kind::Terminal));
            }
            _ => {}
        }
    }
    out
}

/// Achados de um arquivo (cada literal uma vez, no primeiro caminho que o contém).
fn scan_file(file: &str, toks: &[Token], consts: &dyn Fn(&str) -> Option<String>) -> Vec<Hit> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut regions = person_regions(toks);
    regions.extend(provider_reason_regions(file, toks));
    for (from, to, kind) in regions {
        for (k, line, text) in texts_in(toks, from, to, consts) {
            if is_person_text(&text) && seen.insert(k) {
                out.push(Hit { file: file.to_string(), line, kind, text });
            }
        }
    }
    out
}

/// Todos os achados de `src/`, com as constantes resolvidas no arquivo e, sem ambiguidade, no
/// projeto inteiro.
fn scan_sources(sources: &[(String, String)]) -> Vec<Hit> {
    let parsed: Vec<(&str, Vec<Token>)> = sources.iter().map(|(rel, text)| (rel.as_str(), production_tokens(text))).collect();
    let mut global: HashMap<String, Vec<String>> = HashMap::new();
    for (_, toks) in &parsed {
        for (name, text) in str_consts(toks) {
            global.entry(name).or_default().push(text);
        }
    }
    let mut out = Vec::new();
    for (rel, toks) in &parsed {
        if SKIPPED_FILES.iter().any(|(f, _)| f == rel) {
            continue;
        }
        let local: HashMap<String, String> = str_consts(toks).into_iter().collect();
        let consts = |name: &str| {
            local.get(name).cloned().or_else(|| match global.get(name).map(Vec::as_slice) {
                Some([only]) => Some(only.clone()),
                _ => None,
            })
        };
        out.extend(scan_file(rel, toks, &consts));
    }
    out
}

/// Exceção que cobre o achado: a de trecho mais longo, quando mais de uma casa.
fn allowed_by(hit: &Hit) -> Option<usize> {
    EXCEPTIONS
        .iter()
        .enumerate()
        .filter(|(_, e)| e.file == hit.file && e.kind == hit.kind && hit.text.contains(e.snippet))
        .max_by_key(|(_, e)| e.snippet.len())
        .map(|(k, _)| k)
}

#[test]
fn person_text_detector_catches_pt_and_en_and_lets_codes_pass() {
    for text in ["projeto não encontrado", "nao foi possivel", "project not found", "Payload Too Large", "{x}: tarefa {id}", "falha: {e}"] {
        assert!(is_person_text(text), "{text}");
    }
    for text in ["host_forbidden", "run.stopped", "{}: {}", "{prefix} {detail}", "runner.block.docker", "git", "https://a.b/c d", "--port", "{x:?}"] {
        assert!(!is_person_text(text), "{text}");
    }
}

#[test]
fn sweep_finds_each_person_path() {
    let source = r#"
const MSG: &str = "spec não encontrada";
fn f() {
    let a = ApiError::new(StatusCode::NOT_FOUND, MSG);
    let b = ApiError::unprocessable(format!("valor inválido: {x}"));
    emit(RunnerEvent::AgentMessage { agent: "x".into(), text: "Olá, pessoa".into(), task_id: None });
    emit(RunnerEvent::SystemNotice { code: "a.b".into(), text: format!("Aviso: {e}"), params });
    self.say(format!("Revisor: aprovado {x}"));
    self.final_notice("AVISO: falhou".into(), "final.skipped", &[]);
    deliver(&bus, None, ev);
    let j = json!({"agent": "system", "text": "Execution stopped by the user"});
    meta.next_action = Some("aguardando developer".into());
    let m = Meta { next_action: Some("integrar agora".into()), ..x };
    eprintln!("nants: could not open the browser");
    let ok = i18n::tr(lang, "runner.block.docker", &[("reason", "texto livre aqui")]);
    let forbidden = |msg: &str| ApiError::new(StatusCode::FORBIDDEN, msg);
    let r = forbidden("origem recusada");
    let c = conflict("já existe uma execução");
    let e = Rejection::new(StatusCode::CONFLICT, "outra rejeição aqui");
    let body = json!({"error": "corpo malformado", "code": "bad_body"});
}
fn conflict(message: &str) -> Rejection { Rejection::new(StatusCode::CONFLICT, message) }
#[cfg(test)]
mod t { fn g() { let a = ApiError::new(S, "de teste não conta"); } }
"#;
    let toks = production_tokens(source);
    let consts: HashMap<String, String> = str_consts(&toks).into_iter().collect();
    let hits = scan_file("x.rs", &toks, &|n| consts.get(n).cloned());
    let got: Vec<(Kind, &str)> = hits.iter().map(|h| (h.kind, h.text.as_str())).collect();
    assert_eq!(
        got,
        vec![
            (Kind::ApiError, "spec não encontrada"),
            (Kind::ApiError, "valor inválido: {x}"),
            (Kind::ConversationText, "Olá, pessoa"),
            (Kind::ConversationText, "Aviso: {e}"),
            (Kind::Say, "Revisor: aprovado {x}"),
            (Kind::FinalNotice, "AVISO: falhou"),
            (Kind::JsonText, "Execution stopped by the user"),
            (Kind::NextAction, "aguardando developer"),
            (Kind::NextAction, "integrar agora"),
            (Kind::Terminal, "nants: could not open the browser"),
            (Kind::ApiError, "origem recusada"),
            (Kind::ApiError, "já existe uma execução"),
            (Kind::ApiError, "outra rejeição aqui"),
            (Kind::ApiError, "corpo malformado"),
        ]
    );
}

/// Spec 119: a trava do motivo do provedor pega o rótulo fixo no corpo das funções da lista e
/// deixa passar o código estável.
#[test]
fn provider_reason_sweep_catches_fixed_labels() {
    let source = r#"
impl BadKind {
    fn label(self) -> &'static str { match self { BadKind::NoFinal => "transmissão sem evento final", _ => "no_final" } }
}
fn other() -> &'static str { "fora da lista não conta aqui" }
"#;
    let toks = production_tokens(source);
    let hits = scan_file("agents/llm_client/bad_reason.rs", &toks, &|_| None);
    let got: Vec<(Kind, &str)> = hits.iter().map(|h| (h.kind, h.text.as_str())).collect();
    assert_eq!(got, vec![(Kind::ProviderReason, "transmissão sem evento final")]);
    // Toda função da lista existe: a trava não fica sem alvo.
    let sources = production_sources();
    for (file, name) in PROVIDER_REASON_FNS {
        let (_, text) = sources.iter().find(|(rel, _)| rel == file).unwrap_or_else(|| panic!("{file} não existe"));
        let toks = production_tokens(text);
        assert!(!provider_reason_regions(file, &toks).is_empty(), "{file}: fn {name} não existe");
        let found = toks.windows(2).any(|w| w[0].is_ident("fn") && w[1].is_ident(name));
        assert!(found, "{file}: fn {name} não existe");
    }
}

#[test]
fn no_person_text_outside_the_dictionaries() {
    let sources = production_sources();
    assert!(sources.len() > 80, "varredura sem arquivos: {}", sources.len());
    for (file, why) in SKIPPED_FILES {
        assert!(sources.iter().any(|(rel, _)| rel == file), "arquivo ignorado não existe: {file}");
        assert!(why.len() > 20, "{file}: motivo curto");
    }
    for e in EXCEPTIONS {
        assert!(e.why.len() > 20, "{}: {}: motivo curto", e.file, e.snippet);
        assert!(!e.snippet.trim().is_empty(), "{}: trecho vazio", e.file);
    }
    let hits = scan_sources(&sources);
    // A varredura chega aos caminhos que importam.
    for kind in [Kind::ApiError, Kind::ConversationText, Kind::Say, Kind::NextAction, Kind::Terminal] {
        assert!(hits.iter().any(|h| h.kind == kind) || EXCEPTIONS.iter().all(|e| e.kind != kind), "{kind:?}");
    }
    let mut used = vec![false; EXCEPTIONS.len()];
    let mut leaks = Vec::new();
    for hit in &hits {
        match allowed_by(hit) {
            Some(k) => used[k] = true,
            None => leaks.push(format!("src/{}:{}: {:?}: {:?}", hit.file, hit.line, hit.kind, hit.text.chars().take(120).collect::<String>())),
        }
    }
    assert!(leaks.is_empty(), "texto de pessoa fora do dicionário ({}):\n{}", leaks.len(), leaks.join("\n"));
    let stale: Vec<String> = EXCEPTIONS.iter().zip(&used).filter(|(_, u)| !**u).map(|(e, _)| format!("{}: {:?}: {}", e.file, e.kind, e.snippet)).collect();
    assert!(stale.is_empty(), "exceções sem uso (apague da lista):\n{}", stale.join("\n"));
}
