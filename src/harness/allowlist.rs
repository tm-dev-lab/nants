//! Avaliador puro da allowlist de comandos (BLUEPRINT §6.2).
//!
//! Não executa nada e não faz I/O: recebe um comando e uma [`Policy`] e
//! devolve a [`Decision`]. A execução e a aprovação humana ficam em outros módulos.
//!
//! # Natureza do módulo
//!
//! Esta é uma defesa em profundidade para o `run_command` do agente. **Não é uma sandbox** e não
//! é uma fronteira de segurança: as fronteiras reais são a worktree isolada, as ferramentas
//! permitidas por papel e a aprovação humana (HITL). Um avaliador de texto de shell nunca
//! substitui essas fronteiras.
//!
//! ## Garantias
//! - Padrão `denied` que casa (literal, com basename, sem aspas, ou em sufixos de wrapper) vence
//!   qualquer `allowed`.
//! - Escape (`\`), expansão (`$`), crase, `<`, `>`, `&` isolado, quebra de linha, caracteres fora
//!   da whitelist fora de aspas, palavras reservadas do shell e til no início de token → `Deny`.
//!   Nunca viram `Allow`.
//! - Separadores `;`, `&&`, `||`, `|` dividem subcomandos; `Allow` exige que todos casem com `allowed`.
//! - Wrappers (`sh`, `env`, `sudo`, `find -exec`, `xargs`...), atribuições de ambiente (`FOO=1 cmd`)
//!   e interpretadores/executores de código (`python*`, `perl`, `awk`, `sed`, `node`, `make`...)
//!   nunca viram `Allow`: no máximo `NeedsApproval`. Para eles, `denied` também é checado em todos
//!   os sufixos a partir do 2º token e em trechos literais do código embutido (`awk 'system("rm -rf x")'`).
//! - Comandos com mais de 4096 bytes → `Deny` antes de qualquer processamento.
//! - Comparação de padrões é case sensitive; padrões são normalizados em espaços.
//!
//! ## Limitações
//! - A lista de wrappers e interpretadores é fechada: um executável não listado, com nome arbitrário,
//!   só é barrado pelos padrões `denied` ou pela aprovação humana.
//! - Não inspeciona o código embutido em comandos permitidos por glob; um `allowed` amplo
//!   (ex.: `cargo *`) é decisão do usuário e aceita o risco correspondente.
//! - Não resolve `PATH`, links simbólicos nem o conteúdo de scripts executados.

use crate::sdd::Settings;

/// Políticas de comando: padrões glob permitidos e negados.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    allowed: Vec<String>,
    denied: Vec<String>,
}

impl Policy {
    /// Cria uma política a partir de listas de padrões glob.
    pub fn new(allowed: Vec<String>, denied: Vec<String>) -> Self {
        Self {
            allowed: allowed.iter().map(|p| normalize(p)).collect(),
            denied: denied.iter().map(|p| normalize(p)).collect(),
        }
    }

    /// Cria uma política a partir de `allowed_commands` e `denied_commands` de `.sdd/settings.yaml`.
    pub fn from_settings(settings: &Settings) -> Self {
        Self::new(
            settings.allowed_commands.clone(),
            settings.denied_commands.clone(),
        )
    }
}

/// Resultado da avaliação de um comando.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Todos os subcomandos casam com `allowed` e nenhum com `denied`.
    Allow,
    /// Rejeitado imediatamente; a string explica o motivo.
    Deny(String),
    /// Não há regra que permita o comando; depende de aprovação humana.
    NeedsApproval,
}

/// Avalia um comando contra a política. Política conservadora: na dúvida, não há `Allow`.
pub fn evaluate(command: &str, policy: &Policy) -> Decision {
    if command.len() > MAX_COMMAND_BYTES {
        return Decision::Deny("comando longo demais".to_string());
    }
    let normalized = normalize(command);
    if normalized.is_empty() {
        return Decision::Deny("comando vazio".to_string());
    }
    if policy.denied.iter().any(|p| glob_match(p, &normalized)) {
        return Decision::Deny("comando negado por denied_commands".to_string());
    }

    let subcommands = match split_subcommands(command) {
        Ok(subs) => subs,
        Err(reason) => return Decision::Deny(reason),
    };

    // Primeiro passo: qualquer subcomando negado vence, independentemente da ordem.
    for sub in &subcommands {
        if let Some(reason) = structural_reason(sub) {
            return Decision::Deny(reason);
        }
        if let Some(reason) = denied_reason(sub, policy) {
            return Decision::Deny(reason);
        }
    }

    // Segundo passo: `Allow` só se todos os subcomandos casarem com `allowed` e nenhum for wrapper.
    let all_allowed = subcommands.iter().all(|sub| {
        let unquoted = normalize(&sub.replace(['"', '\''], ""));
        !is_wrapper(&unquoted) && policy.allowed.iter().any(|p| glob_match(p, sub))
    });
    if all_allowed {
        Decision::Allow
    } else {
        Decision::NeedsApproval
    }
}

/// `true` se algum padrão `denied` da política casa com o comando (só a denylist; não considera
/// as recusas estruturais de [`evaluate`] por `$`, redirecionamento etc.). Usado pelo YOLO.
pub fn denylist_hit(command: &str, policy: &Policy) -> bool {
    // Comando acima do limite nunca é decidido pelo YOLO (nem pelo juiz): vai ao humano.
    if command.len() > MAX_COMMAND_BYTES {
        return true;
    }
    let normalized = normalize(command);
    if policy.denied.iter().any(|p| glob_match(p, &normalized)) {
        return true;
    }
    match split_subcommands(command) {
        Ok(subs) => subs.iter().any(|sub| denied_reason(sub, policy).is_some()),
        Err(_) => denied_reason(command, policy).is_some(),
    }
}

/// Indica se um comando literal pode ser persistido como padrão `allowed` (aprovação "sempre").
///
/// Exige caracteres do conjunto seguro (`[A-Za-z0-9_./:=@%+,~-]` e espaços), sem aspas, operadores
/// ou `*`, nenhum wrapper/interpretador e `Allow` ao ser avaliado como o próprio padrão. Qualquer
/// outro caso devolve `false` (fail-closed): o comando continua exigindo aprovação.
pub fn is_always_allowable(command: &str) -> bool {
    let normalized = normalize(command);
    if normalized.is_empty() || normalized.len() > MAX_COMMAND_BYTES {
        return false;
    }
    if !normalized.chars().all(|c| c == ' ' || is_plain_char(c)) {
        return false;
    }
    if is_wrapper(&normalized) {
        return false;
    }
    let policy = Policy::new(vec![normalized.clone()], Vec::new());
    evaluate(&normalized, &policy) == Decision::Allow
}

/// Motivo fixo (para o humano) quando o comando não pode ser liberado com "sempre": interpretador
/// ou wrapper, ou seja, executa código arbitrário.
pub const ALWAYS_REASON_ARBITRARY: &str = "este comando executa código arbitrário";
/// Motivo fixo quando o comando não pode ser liberado com "sempre" por operadores ou aspas.
pub const ALWAYS_REASON_CHARS: &str = "este comando contém operadores ou aspas";
/// Motivo genérico quando o comando não pode ser liberado com "sempre" por outro critério.
pub const ALWAYS_REASON_GENERIC: &str = "não pode ser liberado permanentemente";

/// Motivo fixo pelo qual `command` não pode ser liberado com "sempre"; `None` se `is_always_allowable`.
/// Deve receber o comando completo (sem redação): a classificação depende do texto real.
pub fn always_denied_reason(command: &str) -> Option<&'static str> {
    if is_always_allowable(command) {
        return None;
    }
    let normalized = normalize(command);
    if let Ok(subcommands) = split_subcommands(command) {
        let runs_code = subcommands.iter().any(|sub| {
            let unquoted = normalize(&sub.replace(['"', '\''], ""));
            is_wrapper(&unquoted)
        });
        if runs_code {
            return Some(ALWAYS_REASON_ARBITRARY);
        }
    }
    if !normalized.chars().all(|c| c == ' ' || is_plain_char(c)) {
        return Some(ALWAYS_REASON_CHARS);
    }
    Some(ALWAYS_REASON_GENERIC)
}

/// Chave de concessão em memória (`run` e `test_command`). Remove espaços das pontas e colapsa
/// espaços/tabs repetidos somente FORA de aspas. Dentro de aspas (simples ou duplas, inclusive aspas
/// não fechadas, que tratam o resto como citado) o texto é preservado byte a byte. Fora de aspas,
/// uma barra invertida escapa o caractere seguinte. Não é usada pela avaliação de política (que segue em `normalize`).
pub fn grant_key(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let mut quote: Option<char> = None;
    let mut pending_space = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' && q == '"' {
                out.extend(chars.next());
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            ' ' | '\t' => pending_space = true,
            _ => {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                match c {
                    '\\' => {
                        out.push(c);
                        out.extend(chars.next());
                    }
                    '"' | '\'' => {
                        quote = Some(c);
                        out.push(c);
                    }
                    _ => out.push(c),
                }
            }
        }
    }
    out
}

/// Verifica se um subcomando casa com `denied`, considerando caminho absoluto (`/bin/rm` → `rm`),
/// remoção de aspas e, para wrappers, qualquer sufixo a partir do 2º token.
fn denied_reason(sub: &str, policy: &Policy) -> Option<String> {
    let stripped = normalize(&sub.replace(['"', '\''], ""));
    let mut candidates = vec![sub.to_string(), with_basename(sub), stripped.clone(), with_basename(&stripped)];

    if is_wrapper(&stripped) {
        let tokens: Vec<&str> = stripped.split(' ').collect();
        for start in 1..tokens.len() {
            let suffix = tokens[start..].join(" ");
            candidates.push(with_basename(&suffix));
            candidates.push(suffix);
        }
        // Código embutido em wrappers/interpretadores: basta o texto conter os trechos literais do padrão.
        if let Some(p) = policy
            .denied
            .iter()
            .find(|p| contains_literal_pieces(p, &stripped))
        {
            return Some(format!("código negado por `{p}` dentro de wrapper"));
        }
    }

    candidates
        .iter()
        .find(|c| policy.denied.iter().any(|p| glob_match(p, c)))
        .map(|c| format!("subcomando `{c}` negado por denied_commands"))
}

/// Verdadeiro se os trechos do padrão (separados por `*`) aparecem em `text`, na mesma ordem.
fn contains_literal_pieces(pattern: &str, text: &str) -> bool {
    let mut rest = text;
    for piece in pattern.split('*').filter(|s| !s.is_empty()) {
        match rest.find(piece) {
            Some(pos) => rest = &rest[pos + piece.len()..],
            None => return false,
        }
    }
    true
}

/// Substitui o primeiro token por seu basename quando ele contém `/`.
fn with_basename(command: &str) -> String {
    let mut parts = command.splitn(2, ' ');
    let first = parts.next().unwrap_or_default();
    let base = first.rsplit('/').next().unwrap_or(first);
    match parts.next() {
        Some(rest) => format!("{base} {rest}"),
        None => base.to_string(),
    }
}

/// Tamanho máximo do comando em bytes; acima disso, `Deny` antes de qualquer processamento.
const MAX_COMMAND_BYTES: usize = 4096;

/// Interpretadores e executores de código. Sempre tratados como wrappers, independentemente de flags.
const INTERPRETERS: &[&str] = &[
    "perl", "ruby", "node", "nodejs", "deno", "bun", "php", "lua", "luajit", "tclsh", "awk", "gawk",
    "mawk", "nawk", "sed", "ed", "ex", "vi", "vim", "emacs", "jq", "gdb", "make", "cmake", "ninja",
    "cargo-script",
];

/// Programas que executam outro comando (ou código arbitrário). Nunca recebem `Allow`.
const WRAPPERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "env", "eval", "exec", "command", "builtin", "xargs", "nohup",
    "time", "timeout", "nice", "ionice", "sudo", "doas", "su", "setsid", "stdbuf", "watch",
];

/// Indica se o subcomando invoca um wrapper (direto, via basename, ou com flags de execução).
fn is_wrapper(sub: &str) -> bool {
    let tokens: Vec<&str> = sub.split(' ').collect();
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = first.rsplit('/').next().unwrap_or(first);

    // Atribuição de ambiente antes do programa (`FOO=1 rm`): trata como wrapper.
    if first.contains('=') && !first.starts_with('=') {
        return true;
    }
    if WRAPPERS.contains(&program) || INTERPRETERS.contains(&program) {
        return true;
    }
    if program.starts_with("python") || program.starts_with("pypy") {
        return true;
    }
    if program == "find" {
        return tokens
            .iter()
            .any(|t| matches!(*t, "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete"));
    }
    false
}

/// Caracteres permitidos fora de aspas (além dos operadores de separação tratados no splitter).
/// Qualquer outro caractere ASCII de controle, espaço em branco que não seja espaço, wildcard,
/// parêntese, chave, `!`, `#`, `^` ou Unicode não ASCII é bloqueado.
fn is_plain_char(c: char) -> bool {
    c == ' ' || c.is_ascii_alphanumeric() || "_./:=@%+,~-".contains(c)
}

/// Primeira palavra do subcomando: até o primeiro espaço fora de aspas (aspas preservadas).
fn first_word_raw(sub: &str) -> &str {
    let mut in_single = false;
    let mut in_double = false;
    for (idx, c) in sub.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ' ' if !in_single && !in_double => return &sub[..idx],
            _ => {}
        }
    }
    sub
}

/// Palavras reservadas do shell que alteram a estrutura do comando.
const RESERVED_WORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "for", "case", "esac", "in",
    "function", "select", "time", "coproc", "[[",
];

/// Verificações estruturais por subcomando (fail-closed): palavras reservadas, til no início de
/// token e programa entre aspas com caracteres fora da whitelist.
fn structural_reason(sub: &str) -> Option<String> {
    let first_raw = first_word_raw(sub);
    let first_unquoted = first_raw.replace(['"', '\''], "");
    if first_unquoted.chars().any(char::is_whitespace) {
        return Some("programa com espaço entre aspas não suportado".to_string());
    }
    let first = first_unquoted.as_str();
    if RESERVED_WORDS.contains(&first) {
        return Some("palavra reservada do shell".to_string());
    }
    if first_raw.contains(['"', '\'']) && !first.chars().all(is_plain_char) {
        return Some("programa entre aspas com caractere não suportado".to_string());
    }
    if sub.split(' ').any(|t| t.starts_with('~')) {
        return Some("expansão de til não suportada".to_string());
    }
    None
}

/// Normaliza espaços: remove bordas e colapsa sequências de espaços em um só.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Divide o comando em subcomandos por `;`, `&&`, `||` e `|` fora de aspas simples.
///
/// Operadores inseguros (crase, `$(`, `<(`, `>`, `<`, `&` isolado, quebra de linha)
/// fora de aspas simples resultam em erro com a razão do bloqueio.
/// Dentro de aspas duplas, `;`, `|` e `&` são literais, mas crase e `$(` continuam inseguros.
fn split_subcommands(command: &str) -> Result<Vec<String>, String> {
    const UNSAFE: &str = "operador inseguro";

    let chars: Vec<char> = command.chars().collect();
    let mut segments: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();

        if in_single {
            if c == '\'' {
                in_single = false;
            }
            current.push(c);
            i += 1;
            continue;
        }

        // Operadores de separação: consomem o caractere (ou o par `&&`/`||`) e encerram o segmento.
        let separator_len = match c {
            '\n' | '\r' | '`' => return Err(UNSAFE.to_string()),
            '\\' => return Err("escape não suportado".to_string()),
            '$' => return Err("expansão não suportada".to_string()),
            '<' | '>' if !in_double => return Err(UNSAFE.to_string()),
            _ if !in_double
                && !matches!(c, '\'' | '"' | ';' | '|' | '&')
                && !is_plain_char(c) =>
            {
                return Err("caractere não suportado fora de aspas".to_string());
            }
            '&' if !in_double => {
                if next == Some('&') {
                    2
                } else {
                    return Err(UNSAFE.to_string());
                }
            }
            ';' | '|' if !in_double => {
                if c == '|' && next == Some('|') {
                    2
                } else {
                    1
                }
            }
            _ => 0,
        };

        if separator_len > 0 {
            segments.push(std::mem::take(&mut current));
            i += separator_len;
            continue;
        }

        match c {
            '"' => in_double = !in_double,
            '\'' if !in_double => in_single = true,
            _ => {}
        }
        current.push(c);
        i += 1;
    }

    if in_single || in_double {
        return Err("aspas não fechadas".to_string());
    }
    segments.push(current);

    let mut result = Vec::with_capacity(segments.len());
    for segment in segments {
        let normalized = normalize(&segment);
        if normalized.is_empty() {
            return Err("subcomando vazio".to_string());
        }
        result.push(normalized);
    }
    Ok(result)
}

/// Casamento glob ancorado: `*` casa qualquer sequência (inclusive vazia); os demais caracteres são literais.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut star_text = 0usize;

    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_text = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            star_text += 1;
            ti = star_text;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

const YOLO_NOT_AUTO: &str = "comando fora da aprovação automática (exclusão de princípio)";
const YOLO_PRIVILEGE: &[&str] = &["sudo", "doas", "su", "pkexec", "run0"];
const YOLO_SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish", "csh", "tcsh", "eval", "source"];
/// Pastas que o agente nunca altera por comando automático (relativas à raiz).
const YOLO_PROTECTED_DIRS: &[&str] = &[".git", ".sdd", ".nants"];

/// Palavra de um comando: texto, se abre um segmento (início ou depois de `; | &`).
struct YoloWord {
    text: String,
    head: bool,
}

/// Quebra o comando em palavras respeitando aspas; `; | & < > ( )` fora de aspas separam palavras.
/// `None` se as aspas não fecham.
fn yolo_words(command: &str) -> Option<Vec<YoloWord>> {
    let mut out: Vec<YoloWord> = Vec::new();
    let (mut cur, mut has) = (String::new(), false);
    let mut head = true;
    let mut quote: Option<char> = None;
    // Dentro de `$( ... )` ou de crases o texto fica na mesma palavra (é uma expansão só).
    let (mut depth, mut tick) = (0usize, false);
    for c in command.chars() {
        if quote.is_none() {
            if depth > 0 || tick {
                cur.push(c);
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    '`' => tick = false,
                    _ => {}
                }
                continue;
            }
            if c == '`' || (c == '(' && cur.ends_with('$')) {
                cur.push(c);
                has = true;
                if c == '`' {
                    tick = true;
                } else {
                    depth = 1;
                }
                continue;
            }
        }
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    has = true;
                }
                c if c.is_whitespace() || ";|&<>()".contains(c) => {
                    if has {
                        out.push(YoloWord { text: std::mem::take(&mut cur), head });
                        has = false;
                        head = false;
                    }
                    if ";|&(".contains(c) {
                        head = true;
                    }
                }
                c => {
                    cur.push(c);
                    has = true;
                }
            },
        }
    }
    if quote.is_some() || depth > 0 || tick {
        return None;
    }
    if has {
        out.push(YoloWord { text: cur, head });
    }
    Some(out)
}

fn base_name(word: &str) -> &str {
    word.trim_start_matches('\\').rsplit('/').next().unwrap_or(word)
}

/// Mensagem estática devolvida ao agente quando o comando pede elevação de privilégio (spec 071).
pub const PRIVILEGE_REFUSED_MSG: &str = "Elevação de privilégio não é permitida: o Nants não altera o sistema do host. Para dependências de sistema, use o ambiente docker do projeto (o Arquiteto pode propor) ou a ferramenta de navegador em container; pergunte ao Arquiteto com ask_architect se precisar.";

/// Programas que só repassam a execução a outro comando (o programa real vem depois).
const PRIV_PASSTHROUGH: &[&str] =
    &["env", "command", "exec", "builtin", "nohup", "time", "timeout", "nice", "ionice", "setsid", "stdbuf", "watch", "xargs"];

/// `true` se algum comando do texto (programa inicial ou cada comando de pipeline, `&&`, `;`,
/// atrás de `env`/`nohup`/`timeout`..., caminho absoluto ou `sh -c "..."`) é elevação de privilégio.
/// `grep sudo README` (sudo só como argumento) não conta. Vale em qualquer modo (spec 071).
pub fn invokes_privilege_escalation(command: &str) -> bool {
    program_in(command, YOLO_PRIVILEGE, 0)
}

/// Ferramentas de orquestração de container (spec 092 T05): dentro do container do projeto não
/// fazem sentido; o comando deve rodar direto em /work.
const CONTAINER_TOOLS: &[&str] = &["docker", "docker-compose", "podman", "kubectl", "compose"];

/// Mensagem estática devolvida ao Desenvolvedor quando, já dentro do container, o comando invoca
/// `docker`/`compose`/`podman`/`kubectl` (spec 092 T05).
pub const CONTAINER_TOOL_REFUSED_MSG: &str =
    "você já está dentro do container do projeto (/work); rode o comando diretamente, sem docker/compose";

/// `true` se algum comando do texto invoca `docker`, `docker-compose`, `podman`, `kubectl` ou
/// `compose` como PROGRAMA (mesmas posições de [`invokes_privilege_escalation`]). `grep docker
/// README` (só como argumento) não conta.
pub fn invokes_container_tool_program(command: &str) -> bool {
    program_in(command, CONTAINER_TOOLS, 0)
}

/// `true` se algum programa do texto (início de segmento, atrás de repassadores ou `sh -c "..."`)
/// tem o nome-base em `names`.
fn program_in(command: &str, names: &[&str], depth: usize) -> bool {
    let Some(words) = yolo_words(command) else {
        // Aspas abertas: sem como separar com segurança; olha só o início de cada trecho.
        return command
            .split(|c| ";|&\n(".contains(c))
            .any(|seg| seg.split_whitespace().next().is_some_and(|w| names.contains(&base_name(w))));
    };
    let mut i = 0;
    while i < words.len() {
        if !words[i].head {
            i += 1;
            continue;
        }
        let end = words[i + 1..].iter().position(|w| w.head).map_or(words.len(), |p| i + 1 + p);
        let seg = &words[i..end];
        let mut j = 0;
        while j < seg.len() {
            let t = seg[j].text.as_str();
            let b = base_name(t);
            if names.contains(&b) {
                return true;
            }
            if YOLO_SHELLS.contains(&b) {
                if depth < 4 {
                    if let Some(k) = seg[j + 1..].iter().position(|w| w.text.starts_with('-') && w.text.contains('c')) {
                        if let Some(inner) = seg.get(j + 1 + k + 1) {
                            if program_in(&inner.text, names, depth + 1) {
                                return true;
                            }
                        }
                    }
                }
                break;
            }
            let skippable = PRIV_PASSTHROUGH.contains(&b)
                || (j > 0 && (t.starts_with('-') || t.contains('=') || t.chars().all(|c| c.is_ascii_digit() || c == '.' || c == 's' || c == 'm')))
                || (j == 0 && t.contains('=') && !t.starts_with('='));
            if !skippable {
                break;
            }
            j += 1;
        }
        i = end;
    }
    false
}

/// Exclusões de PRINCÍPIO do YOLO no host (spec 062), sem lista de programas: caracteres de controle,
/// privilégio (`sudo`/`su`/`doas`/`pkexec`/`run0` em qualquer posição), git de escrita remota ou
/// destrutiva (`push`, `remote`, `config`, `clean`, `reset --hard`, `filter-*`, `-c`, `--git-dir`,
/// `--work-tree`), publicação/autenticação (`publish`, `login`...) e instalação global
/// (`-g`/`--global`, `--prefix`, `--location`, `--user`, `cargo/go install`, `config`).
///
/// `--user` (ex.: `pip install --user x`) é recusado: instala em `~/.local`, fora da raiz e no
/// ambiente do usuário, e nenhum argumento-caminho denuncia isso; `pip install x` (sem `--user`)
/// segue automático. Devolve o motivo ou `None`.
pub fn yolo_excluded_reason(command: &str) -> Option<&'static str> {
    if command.trim().is_empty() || command.chars().any(|c| c.is_control() && c != ' ' && c != '\t') {
        return Some(YOLO_NOT_AUTO);
    }
    // Aspas abertas não é exclusão de princípio: o juiz decide (ver `yolo_host_verdict`).
    let words = yolo_words(command)?;
    let args: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    if args.iter().any(|w| YOLO_PRIVILEGE.contains(&base_name(w))) {
        return Some(YOLO_NOT_AUTO);
    }
    let has = |names: &[&str]| args.iter().any(|t| names.contains(t));
    let starts = |t: &str, pre: &[&str]| pre.iter().any(|p| t.starts_with(p));
    if args.iter().any(|w| base_name(w) == "git")
        && (has(&["push", "remote", "config", "clean", "filter-branch", "filter-repo", "-c"])
            || args.iter().any(|t| starts(t, &["filter-", "--git-dir", "--work-tree"]))
            || (has(&["reset"]) && has(&["--hard"])))
    {
        return Some(YOLO_NOT_AUTO);
    }
    if args.iter().any(|t| matches!(*t, "--user" | "--global") || starts(t, &["--prefix", "--root", "--location", "--userconfig", "--globalconfig", "--global="])) {
        return Some(YOLO_NOT_AUTO);
    }
    if has(&["-g"]) && has(&["install", "i", "add", "update", "upgrade", "remove", "uninstall", "link", "tool"]) {
        return Some(YOLO_NOT_AUTO);
    }
    for (i, w) in words.iter().enumerate() {
        if !w.head {
            continue;
        }
        let prog = base_name(&w.text);
        // Subcomando = primeira palavra que não é opção, logo depois do programa do segmento.
        let sub = words[i + 1..]
            .iter()
            .take_while(|n| !n.head)
            .map(|n| n.text.as_str())
            .find(|t| !t.starts_with('-'))
            .unwrap_or("");
        // Publicar, autenticar ou gravar configuração persistente.
        // Só para CLIs de pacotes: `grep login src` não é publicação.
        const PACKAGE_CLIS: &[&str] = &[
            "npm", "pnpm", "yarn", "npx", "cargo", "gem", "twine", "mvn", "gradle", "dotnet", "poetry", "mix", "bundle", "uv",
            "pip", "pip3", "flit", "composer", "nuget", "docker",
        ];
        if PACKAGE_CLIS.contains(&prog)
            && (matches!(sub, "publish" | "deploy" | "login" | "logout" | "adduser" | "unpublish" | "yank" | "owner" | "token")
                || sub.ends_with(".publish")
                || sub.rsplit(':').next().is_some_and(|t| t.starts_with("publish")))
        {
            return Some(YOLO_NOT_AUTO);
        }
        match (prog, sub) {
            ("cargo" | "go", "install") => return Some(YOLO_NOT_AUTO),
            ("npm" | "pnpm" | "yarn" | "pip" | "pip3", "config" | "set" | "global" | "link") => return Some(YOLO_NOT_AUTO),
            ("dotnet", "nuget") if has(&["push"]) => return Some(YOLO_NOT_AUTO),
            ("go", "env") if args.iter().any(|t| starts(t, &["-w", "-u"])) => return Some(YOLO_NOT_AUTO),
            ("cargo", _) if args.iter().any(|t| *t == "--config" || starts(t, &["--config=", "-Z", "--root", "--install-root"])) => {
                return Some(YOLO_NOT_AUTO)
            }
            _ => {}
        }
    }
    None
}

/// Resultado da checagem do YOLO no host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum YoloHostVerdict {
    /// Nenhuma exclusão nem dúvida: aprova.
    Auto,
    /// Exclusão de princípio ou caminho que sai da raiz / cai em `.git`/`.sdd`/`.nants`: humano.
    /// O motivo é nota interna (testes e depuração): o pedido vai à pessoa sem ele, no fluxo normal
    /// de aprovação, então fica fora do dicionário (spec 115 T04).
    Excluded(String),
    /// Construção que as checagens não decidem (`$(`, crase, `$VAR`, shell, aspas abertas, caminho
    /// em texto citado): vai ao juiz.
    NeedsJudge(JudgeDoubt),
}

/// Dúvida que leva um comando ao juiz (spec 115 T03). O texto sai pelo dicionário, no idioma de
/// quem lê: o juiz recebe em pt-BR (o prompt dele é em português) e, sem juiz, a pessoa recebe no
/// idioma do projeto, no aviso de que o pedido foi para ela.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeDoubt {
    /// Aspas não fechadas: o comando não se divide em palavras.
    UnclosedQuotes,
    /// Executa um shell (a palavra do programa).
    Shell(String),
    /// Expansão dinâmica (`$`, crase) na palavra.
    Dynamic(String),
    /// Caminho fora da raiz do projeto.
    OutsideRoot(String),
    /// Caminho dentro de `.git`, `.sdd` ou `.nants`.
    Protected(String),
    /// A raiz do projeto não pôde ser resolvida no disco.
    RootUnresolvable,
    /// O argumento abrange a raiz ou uma pasta que contém `.git`, `.sdd` ou `.nants`.
    CoversRoot(String),
}

impl JudgeDoubt {
    /// Texto da dúvida em `lang`.
    pub fn text(&self, lang: crate::i18n::Lang) -> String {
        let (key, param) = match self {
            Self::UnclosedQuotes => ("allowlist.doubt.unclosed_quotes", None),
            Self::Shell(word) => ("allowlist.doubt.shell", Some(("word", word))),
            Self::Dynamic(word) => ("allowlist.doubt.dynamic", Some(("word", word))),
            Self::OutsideRoot(path) => ("allowlist.doubt.outside_root", Some(("path", path))),
            Self::Protected(path) => ("allowlist.doubt.protected", Some(("path", path))),
            Self::RootUnresolvable => ("allowlist.doubt.root_unresolvable", None),
            Self::CoversRoot(path) => ("allowlist.doubt.covers_root", Some(("path", path))),
        };
        let params: Vec<(&str, &str)> = param.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::i18n::tr(lang, key, &params)
    }
}

/// Resolve `raw` no disco relativo à raiz: cada componente existente é canonicalizado (symlinks e
/// `..` resolvidos), o resto é normalizado lexicalmente. Erro com o motivo se sai da raiz ou cai
/// em pasta protegida.
///
/// `Ok(Some(motivo))`: o argumento resolve para a raiz ou para um ancestral de pasta protegida
/// existente (`.`, `./`, `src/..`): um comando destrutivo atingiria `.git`/`.sdd`/`.nants`; o juiz decide.
fn yolo_resolve(root: &std::path::Path, raw: &str) -> Result<Option<JudgeDoubt>, JudgeDoubt> {
    use std::path::{Component, Path, PathBuf};
    if raw.is_empty() || raw == "/dev/null" {
        return Ok(None);
    }
    if raw.starts_with('~') {
        return Err(JudgeDoubt::OutsideRoot(raw.to_string()));
    }
    let canon_root = root.canonicalize().map_err(|_| JudgeDoubt::RootUnresolvable)?;
    let path = if Path::new(raw).is_absolute() { PathBuf::from(raw) } else { canon_root.join(raw) };
    let mut cur = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => cur.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                cur.pop();
            }
            Component::Normal(name) => {
                cur.push(name);
                if let Ok(real) = cur.canonicalize() {
                    cur = real;
                }
            }
        }
    }
    let Ok(rel) = cur.strip_prefix(&canon_root) else {
        return Err(JudgeDoubt::OutsideRoot(raw.to_string()));
    };
    let protected = rel.components().any(|c| match c {
        Component::Normal(n) => YOLO_PROTECTED_DIRS.iter().any(|p| n == std::ffi::OsStr::new(p)),
        _ => false,
    });
    if protected {
        return Err(JudgeDoubt::Protected(raw.to_string()));
    }
    if rel.as_os_str().is_empty() || YOLO_PROTECTED_DIRS.iter().any(|p| cur.join(p).exists()) {
        return Ok(Some(JudgeDoubt::CoversRoot(raw.to_string())));
    }
    Ok(None)
}

/// Decisão do YOLO no host (spec 062): exclusões de princípio, depois fatos do disco para cada
/// argumento que parece caminho (relativo a `root`), depois construções que o texto não decide
/// (juiz). Sem lista de programas.
pub fn yolo_host_verdict(command: &str, root: &std::path::Path) -> YoloHostVerdict {
    if command.len() > MAX_COMMAND_BYTES {
        return YoloHostVerdict::Excluded("comando longo demais".to_string());
    }
    if let Some(reason) = yolo_excluded_reason(command) {
        return YoloHostVerdict::Excluded(reason.to_string());
    }
    let Some(words) = yolo_words(command) else {
        return YoloHostVerdict::NeedsJudge(JudgeDoubt::UnclosedQuotes);
    };
    let mut judge: Option<JudgeDoubt> = None;
    for word in &words {
        let w = word.text.as_str();
        if word.head && YOLO_SHELLS.contains(&base_name(w)) {
            judge.get_or_insert(JudgeDoubt::Shell(w.to_string()));
        }
        if w.contains('$') || w.contains('`') {
            judge.get_or_insert(JudgeDoubt::Dynamic(w.to_string()));
            continue;
        }
        // Candidatos a caminho: o valor de `--opt=valor`/`VAR=valor`, o argumento inteiro e, para
        // opção curta com valor anexado, o resto depois da letra. Texto citado com espaços é
        // varrido palavra a palavra; achado ali vai ao juiz (pode ser só prosa).
        let multiword = w.contains(char::is_whitespace);
        let mut candidates: Vec<String> = Vec::new();
        if multiword {
            candidates.extend(w.split_whitespace().filter(|p| !p.starts_with('-')).map(str::to_string));
        } else {
            if let Some((_, value)) = w.split_once('=') {
                candidates.push(value.to_string());
            }
            if !w.starts_with('-') {
                candidates.push(w.to_string());
            } else {
                if w.contains('/') || w.contains("..") {
                    candidates.push(w.trim_start_matches('-').to_string());
                }
                // Opção curta com valor anexado (`-t.sdd`, `-o.nants`): o valor é sempre candidato.
                if !w.starts_with("--") && w.chars().count() > 2 {
                    candidates.push(w.chars().skip(2).collect());
                }
            }
        }
        for cand in candidates {
            match yolo_resolve(root, &cand) {
                Err(why) if multiword => {
                    judge.get_or_insert(why);
                }
                // O motivo da exclusão é interno (o pedido vai à pessoa sem ele).
                Err(why) => return YoloHostVerdict::Excluded(why.text(crate::i18n::Lang::PtBr)),
                Ok(Some(why)) => {
                    judge.get_or_insert(why);
                }
                Ok(None) => {}
            }
        }
    }
    match judge {
        Some(why) => YoloHostVerdict::NeedsJudge(why),
        None => YoloHostVerdict::Auto,
    }
}

/// YOLO para comandos que rodam DENTRO do container (spec 056/062): o isolamento (`.git` e `.nants`
/// somente leitura, worktree em /work) cobre o resto; só exclusões de princípio (privilégio e
/// ferramentas de orquestração). Devolve o motivo da recusa ou `None` se pode.
pub fn yolo_container_excluded_reason(command: &str) -> Option<&'static str> {
    const HARD: &str = "comando fora da aprovação automática (exclusão dura)";
    if command.trim().is_empty() {
        return Some(HARD);
    }
    const PRIV: &[&str] = &["sudo", "doas", "su", "pkexec", "run0"];
    const TOOLS: &[&str] = &["docker", "podman", "docker-compose", "compose", "kubectl"];
    let hit = command
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'))
        .any(|w| PRIV.contains(&w) || TOOLS.contains(&w));
    hit.then_some(HARD)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allowed: &[&str], denied: &[&str]) -> Policy {
        Policy::new(
            allowed.iter().map(|s| s.to_string()).collect(),
            denied.iter().map(|s| s.to_string()).collect(),
        )
    }

    fn is_deny(d: &Decision) -> bool {
        matches!(d, Decision::Deny(_))
    }

    fn yolo_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("nants-yolo-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).expect("git dir");
        std::fs::create_dir_all(root.join(".sdd")).expect("sdd dir");
        std::fs::create_dir_all(root.join("src")).expect("src dir");
        std::fs::write(root.join(".gitignore"), "target/\n").expect("gitignore");
        root
    }

    #[test]
    fn container_tool_detected_as_program_but_not_as_argument() {
        for cmd in [
            "docker compose -f .nants/compose.yaml run --rm workspace npm test", "docker-compose up -d", "podman ps",
            "kubectl get pods", "compose up", "npm ci && docker build .", "/usr/bin/docker ps", "env FOO=1 docker ps",
            "timeout 5 docker ps", "bash -c \"docker ps\"", "ls | xargs docker rm",
        ] {
            assert!(invokes_container_tool_program(cmd), "{cmd}");
        }
        for cmd in ["npm test", "grep docker README", "cat docker-compose.yml", "echo \"docker ps\"", "ls compose"] {
            assert!(!invokes_container_tool_program(cmd), "{cmd}");
        }
    }

    #[test]
    fn privilege_escalation_detected_per_command_but_not_as_text_argument() {
        for cmd in [
            "sudo npx playwright install-deps chromium", "ls | sudo tee x", "npm ci && sudo apt install x", "a; doas ls",
            "/usr/bin/sudo ls", "env sudo ls", "env FOO=1 sudo ls", "FOO=1 sudo ls", "timeout 5 sudo ls", "doas ls",
            "bash -c \"sudo ls\"", "pkexec x", "nohup sudo x &", "\\sudo ls",
        ] {
            assert!(invokes_privilege_escalation(cmd), "{cmd}");
        }
        for cmd in ["grep sudo README", "echo \"sudo ls\"", "cat sudoers.md", "npm test", "ls | grep su"] {
            assert!(!invokes_privilege_escalation(cmd), "{cmd}");
        }
    }

    #[test]
    fn yolo_host_approves_any_program_without_a_list() {
        let root = yolo_root("auto");
        for cmd in [
            "mix test", "clojure -M:test", "bundle exec rake", "nl -ba src/x.clj", "mix deps.get", "npm install",
            "npm test", "pip install requests", "pip install -r requirements.txt", "cargo test", "go test ./...",
            "git status", "git add -A", "git commit -m feat", "git checkout -- src/a.rs", "rm -rf target/debug",
            "mkdir -p src/a", "mv a.txt b.txt", "find src -name x", "sort -o out.txt a.txt",
            "grep -F target/ .gitignore", "cat .gitattributes", "node scripts/x.js", "python -c print(1)",
            "git commit -m \"fix: a b\"", "ls > out.txt", "cat a.txt 2>/dev/null",
        ] {
            assert_eq!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Auto, "{cmd}");
        }
    }

    #[test]
    fn yolo_host_resolves_paths_on_disk() {
        let root = yolo_root("paths");
        let outside = std::env::temp_dir().join(format!("nants-yolo-outside-{}", std::process::id()));
        std::fs::create_dir_all(&outside).expect("outside");
        std::os::unix::fs::symlink(&outside, root.join("escape")).expect("symlink");
        std::os::unix::fs::symlink(root.join(".git"), root.join("gitlink")).expect("symlink git");
        for cmd in [
            "cat ../fora", "rm -rf /tmp/../home", "cat escape/x", "cat escape", "cat gitlink/config", "cat .git/config",
            "rm -rf .git", "ls .sdd/specs", "cat .nants/compose.yaml", "cat x/.git/HEAD", "cat ./.git/HEAD",
            "cat --f=.git", "cat -I../x", "rm -rf ~/x", "rm -rf /", "cat /etc/passwd", "ls > ../out", "ls src/../../x",
            "cp a /etc/x", "mv a ../b", "FOO=/etc/x make", "cat src/../.sdd/settings.yaml",
        ] {
            assert!(matches!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Excluded(_)), "{cmd}");
        }
    }

    #[test]
    fn yolo_host_root_and_ancestors_go_to_the_judge() {
        let root = yolo_root("ancestors");
        for cmd in ["rm -rf .", "rm -rf ./", "rm -rf src/..", "mv . x", "find . -delete"] {
            assert!(matches!(yolo_host_verdict(cmd, &root), YoloHostVerdict::NeedsJudge(_)), "{cmd}");
        }
        for cmd in ["ls src", "grep -r x src", "rm -rf src/a"] {
            assert_eq!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Auto, "{cmd}");
        }
    }

    #[test]
    fn yolo_host_short_option_attached_values_are_paths() {
        let root = yolo_root("short");
        for cmd in ["cp a -t.sdd", "cp a -t.git", "ls -o.nants", "cp a -t../x"] {
            assert!(matches!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Excluded(_)), "{cmd}");
        }
        for cmd in ["ls -la src", "clojure -M:test", "grep -rn x src"] {
            assert_eq!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Auto, "{cmd}");
        }
    }

    #[test]
    fn yolo_oversized_command_is_never_automatic() {
        let root = yolo_root("big");
        let big = format!("echo {}", "a".repeat(MAX_COMMAND_BYTES + 1));
        assert!(matches!(yolo_host_verdict(&big, &root), YoloHostVerdict::Excluded(_)));
        assert!(denylist_hit(&big, &policy(&[], &[])));
    }

    #[test]
    fn yolo_host_principle_exclusions() {
        let root = yolo_root("principle");
        for cmd in [
            "sudo apt install x", "env sudo ls", "\\sudo ls", "/usr/bin/sudo ls", "su -c x", "doas ls", "pkexec x",
            "git push origin main", "git -C . push --force", "git remote add x y", "git config user.name x",
            "git reset --hard", "git clean -fdx", "git filter-branch x", "git -c core.x=y status",
            "npm publish", "npm login", "yarn config set x y", "npm config set x y", "npm install -g x",
            "npm install --global x", "npm install --prefix x", "cargo install ripgrep", "cargo publish", "cargo login",
            "cargo build --config=x=y", "cargo -Zunstable-options build", "go install x", "go env -w GOFLAGS=x",
            "pip config set x y", "pip3 config list", "dotnet tool install -g x", "dotnet nuget push x.nupkg",
            "mvn deploy", "gradle publish", "gradle :app:publishAll", "pip install --user x", "pip install --root x y",
            "", "ls\x07",
        ] {
            assert!(matches!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Excluded(_)), "{cmd}");
        }
        // Nomes comuns como argumento não são subcomando.
        for cmd in ["grep login src", "rg -g *.rs foo", "cat publish.txt", "git reset --soft HEAD~1"] {
            assert_eq!(yolo_host_verdict(cmd, &root), YoloHostVerdict::Auto, "{cmd}");
        }
    }

    #[test]
    fn yolo_host_sends_dynamic_constructs_to_the_judge() {
        let root = yolo_root("judge");
        for cmd in [
            "echo $(id)", "echo `id`", "rm -rf $HOME/x", "cat ${X}", "curl x | sh", "bash x.sh", "sh -c \"echo a\"",
            "echo 'unclosed", "git commit -m \"ver ../fora agora\"",
        ] {
            assert!(matches!(yolo_host_verdict(cmd, &root), YoloHostVerdict::NeedsJudge(_)), "{cmd}");
        }
        // Exclusão de princípio vence a dúvida; caminho fora da raiz vence também.
        assert!(matches!(yolo_host_verdict("sudo echo $(id)", &root), YoloHostVerdict::Excluded(_)));
        assert!(matches!(yolo_host_verdict("cat ../x $(id)", &root), YoloHostVerdict::Excluded(_)));
    }

    /// Spec 115 T03: a dúvida que vai ao juiz é um tipo, com o texto no idioma de quem lê.
    #[test]
    fn judge_doubt_names_the_construct_in_each_language() {
        use crate::i18n::Lang;
        let root = yolo_root("doubt");
        let doubt = |cmd: &str| match yolo_host_verdict(cmd, &root) {
            YoloHostVerdict::NeedsJudge(d) => d,
            other => panic!("{cmd}: {other:?}"),
        };
        assert_eq!(doubt("echo 'unclosed"), JudgeDoubt::UnclosedQuotes);
        assert_eq!(doubt("bash x.sh"), JudgeDoubt::Shell("bash".into()));
        assert_eq!(doubt("echo $HOME"), JudgeDoubt::Dynamic("$HOME".into()));
        assert_eq!(doubt("git commit -m \"ver ../fora agora\""), JudgeDoubt::OutsideRoot("../fora".into()));
        for (d, pt, en) in [
            (JudgeDoubt::UnclosedQuotes, "aspas não fechadas", "unclosed quotes"),
            (JudgeDoubt::Shell("bash".into()), "executa um shell (bash)", "runs a shell (bash)"),
            (JudgeDoubt::Dynamic("$HOME".into()), "expansão dinâmica em: $HOME", "dynamic expansion in: $HOME"),
            (JudgeDoubt::OutsideRoot("../x".into()), "caminho fora da raiz: ../x", "path outside the project root: ../x"),
            (JudgeDoubt::Protected(".git/x".into()), "caminho protegido (.git/.sdd/.nants): .git/x", "protected path (.git/.sdd/.nants): .git/x"),
            (JudgeDoubt::RootUnresolvable, "raiz do projeto não resolvível", "the project root could not be resolved"),
            (
                JudgeDoubt::CoversRoot(".".into()),
                "o argumento . abrange a raiz ou um diretório que contém .git/.sdd/.nants",
                "the argument . covers the root or a folder that contains .git/.sdd/.nants",
            ),
        ] {
            assert_eq!(d.text(Lang::PtBr), pt);
            assert_eq!(d.text(Lang::En), en);
        }
    }

    #[test]
    fn always_denied_reason_explains_each_case() {
        assert_eq!(always_denied_reason("node --test"), Some(ALWAYS_REASON_ARBITRARY));
        assert_eq!(always_denied_reason("sh -c x"), Some(ALWAYS_REASON_ARBITRARY));
        assert_eq!(always_denied_reason("ls && node -e x"), Some(ALWAYS_REASON_ARBITRARY));
        assert_eq!(always_denied_reason("a && b"), Some(ALWAYS_REASON_CHARS));
        assert_eq!(always_denied_reason("echo \"x\""), Some(ALWAYS_REASON_CHARS));
        assert_eq!(always_denied_reason(&"a".repeat(5000)), Some(ALWAYS_REASON_GENERIC));
        assert_eq!(always_denied_reason("git status --short"), None);
        assert_eq!(always_denied_reason("cargo test"), None);
    }

    #[test]
    fn always_denied_reason_is_none_exactly_when_always_allowable() {
        for cmd in ["node --test", "git status", "a && b", "echo \"x\"", "rm -f x", "sh -c x"] {
            assert_eq!(always_denied_reason(cmd).is_none(), is_always_allowable(cmd), "{cmd}");
        }
    }

    #[test]
    fn grant_key_collapses_spaces_only_outside_quotes() {
        assert_eq!(grant_key("  node   --test "), "node --test");
        assert_eq!(grant_key("echo   a\t\tb"), "echo a b");
        assert_eq!(grant_key("echo \"a  b\""), "echo \"a  b\"");
        assert_eq!(grant_key("echo 'a  b'"), "echo 'a  b'");
        assert_eq!(grant_key("echo \"it's  x\""), "echo \"it's  x\"");
        assert_eq!(grant_key("echo \"a  b"), "echo \"a  b", "aspa não fechada: resto citado");
        assert_eq!(grant_key("echo \\\"a  b"), "echo \\\"a b", "barra escapa aspas fora de aspas");
        assert_ne!(grant_key("echo \"a  b\""), grant_key("echo \"a b\""));
        assert_ne!(grant_key("echo 'a  b'"), grant_key("echo \"a  b\""));
    }

    #[test]
    fn normalize_still_collapses_inside_quotes_for_policy() {
        assert_eq!(normalize("echo \"a  b\""), "echo \"a b\"");
    }

    #[test]
    fn allow_por_glob() {
        let p = policy(&["cargo test*"], &[]);
        assert_eq!(evaluate("cargo test", &p), Decision::Allow);
        assert_eq!(evaluate("cargo test --all -- foo", &p), Decision::Allow);
        assert_eq!(evaluate("  cargo   test  ", &p), Decision::Allow);
    }

    #[test]
    fn deny_vence_allow() {
        let p = policy(&["cargo *"], &["cargo publish*"]);
        assert!(is_deny(&evaluate("cargo publish", &p)));
        assert_eq!(evaluate("cargo build", &p), Decision::Allow);
    }

    #[test]
    fn encadeamento_com_subcomando_negado_ou_desconhecido() {
        let p = policy(&["cargo test*"], &["rm -rf*"]);
        assert!(is_deny(&evaluate("cargo test && rm -rf x", &p)));

        let p = policy(&["cargo test*"], &[]);
        assert_eq!(
            evaluate("cargo test && rm -rf x", &p),
            Decision::NeedsApproval
        );
    }

    #[test]
    fn encadeamento_permitido_e_allow() {
        let p = policy(&["cargo test*", "cargo check*"], &[]);
        assert_eq!(evaluate("cargo test; cargo check", &p), Decision::Allow);
        assert_eq!(evaluate("cargo test || cargo check", &p), Decision::Allow);
        assert_eq!(evaluate("cargo test | cargo check", &p), Decision::Allow);
        assert_eq!(
            evaluate("cargo test; cargo clippy", &p),
            Decision::NeedsApproval
        );
    }

    #[test]
    fn substituicao_e_redirecionamento_sao_deny() {
        let p = policy(&["echo *", "cat *"], &[]);
        assert!(is_deny(&evaluate("echo $(whoami)", &p)));
        assert!(is_deny(&evaluate("echo `whoami`", &p)));
        assert!(is_deny(&evaluate("echo \"$(whoami)\"", &p)));
        assert!(is_deny(&evaluate("cat <(ls)", &p)));
        assert!(is_deny(&evaluate("echo hi > out.txt", &p)));
        assert!(is_deny(&evaluate("echo hi >> out.txt", &p)));
        assert!(is_deny(&evaluate("cat < in.txt", &p)));
        assert!(is_deny(&evaluate("echo a\necho b", &p)));
        assert!(is_deny(&evaluate("echo a & echo b", &p)));
    }

    #[test]
    fn aspas_protegem_separadores() {
        let p = policy(&["echo *"], &[]);
        assert_eq!(evaluate("echo \"a;b\"", &p), Decision::Allow);
        assert_eq!(evaluate("echo 'a;b && c'", &p), Decision::Allow);
        assert_eq!(evaluate("echo 'a $(x) > y'", &p), Decision::Allow);
        assert_eq!(evaluate("echo \"a|b\"", &p), Decision::Allow);
        assert!(is_deny(&evaluate("echo \"unclosed", &p)));
    }

    #[test]
    fn vazio_e_desconhecido() {
        let p = policy(&["cargo test*"], &[]);
        assert!(is_deny(&evaluate("", &p)));
        assert!(is_deny(&evaluate("   ", &p)));
        assert!(is_deny(&evaluate("cargo test;", &p)));
        assert_eq!(evaluate("make install", &p), Decision::NeedsApproval);
    }

    #[test]
    fn case_sensitive() {
        let p = policy(&["cargo test*"], &["Rm *"]);
        assert_eq!(evaluate("Cargo test", &p), Decision::NeedsApproval);
        assert_eq!(evaluate("rm -rf x", &p), Decision::NeedsApproval);
    }

    #[test]
    fn glob_basico() {
        assert!(glob_match("*", "qualquer coisa"));
        assert!(glob_match("a*c", "abc"));
        assert!(glob_match("a*c", "ac"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(!glob_match("a*c", "abd"));
        assert!(!glob_match("abc", "abcd"));
        assert!(glob_match("git status", "git status"));
    }

    #[test]
    fn regressao_f01_escape_com_barra_invertida() {
        let p = policy(&["cargo test*"], &["rm -rf*"]);
        assert!(is_deny(&evaluate("cargo test \\\"; rm -rf x; echo \\\"", &p)));
        let p = policy(&["echo *"], &[]);
        assert!(is_deny(&evaluate("echo \\'; touch /tmp/x; echo \\'", &p)));
        assert!(is_deny(&evaluate("echo \\\"", &p)));
        assert!(is_deny(&evaluate("echo \"a\\\"b\"", &p)));
    }

    #[test]
    fn regressao_f02_expansao_dollar() {
        let p = policy(&["echo *"], &["rm -rf*"]);
        assert!(is_deny(&evaluate("echo $'\\''; rm -rf x; echo $'\\''", &p)));
        assert!(is_deny(&evaluate("echo $HOME", &p)));
        assert!(is_deny(&evaluate("echo \"$HOME\"", &p)));
        assert_eq!(evaluate("echo '$HOME'", &p), Decision::Allow);
    }

    #[test]
    fn regressao_f03_wrappers_e_caminho_absoluto() {
        let p = policy(&["sh *", "env *", "find *"], &["rm -rf*"]);
        assert!(is_deny(&evaluate("sh -c \"rm -rf x\"", &p)));
        assert!(is_deny(&evaluate("env rm -rf x", &p)));
        assert!(is_deny(&evaluate("sudo rm -rf x", &p)));
        assert!(is_deny(&evaluate("/bin/rm -rf x", &p)));
        assert!(is_deny(&evaluate("env /bin/rm -rf x", &p)));
        assert_eq!(evaluate("sh -c \"ls\"", &p), Decision::NeedsApproval);
        assert_eq!(evaluate("env ls", &p), Decision::NeedsApproval);
        assert_eq!(evaluate("find . -delete", &p), Decision::NeedsApproval);
        assert_eq!(evaluate("find . -name x", &p), Decision::Allow);

        let p = policy(&["cargo test*", "python3*"], &[]);
        assert_eq!(evaluate("/usr/bin/cargo test", &p), Decision::NeedsApproval);
        assert_eq!(evaluate("python3 -c \"print(1)\"", &p), Decision::NeedsApproval);
    }

    #[test]
    fn regressao_f05_f06_estrutura_fail_closed() {
        let p = policy(&["echo *"], &["rm -rf*"]);
        let entradas = [
            "{ rm -rf x; }",
            "(rm -rf x)",
            "! rm -rf x",
            "if rm -rf x; then :; fi",
            "while rm -rf x; do :; done",
            "echo ok; time echo hi",
            "echo ok; if echo hi",
            "/bin/r? -rf x",
            "/bin/[r]m -rf x",
            "{rm,-rf,x}",
            "~/bin/rm -rf x",
            "FOO=1 rm -rf x",
            "\"/bin/r?\" -rf x",
            "echo ok\tfoo",
        ];
        for cmd in entradas {
            assert!(is_deny(&evaluate(cmd, &p)), "deveria negar: {cmd}");
        }
    }

    #[test]
    fn regressao_f07_interpretadores_nunca_allow() {
        let p = policy(&["python3*", "awk*", "sed*"], &["rm -rf*"]);
        let neutros = [
            "python3 -Sc \"print(1)\"",
            "python3 -c\"print(1)\"",
            "awk 'BEGIN{print 1}'",
            "sed '1d'",
        ];
        for cmd in neutros {
            assert_eq!(evaluate(cmd, &p), Decision::NeedsApproval, "{cmd}");
        }
        let perigosos = [
            "python3 -Sc \"import os; os.system('rm -rf x')\"",
            "python3 -p rm -rf x",
            "awk 'BEGIN{system(\"rm -rf x\")}'",
            "sed 'e rm -rf x'",
        ];
        for cmd in perigosos {
            assert!(is_deny(&evaluate(cmd, &p)), "deveria negar: {cmd}");
        }
    }

    #[test]
    fn regressao_f08_primeira_palavra_com_espaco_entre_aspas() {
        let p = policy(&["./scripts/*"], &["rm -rf*"]);
        assert!(is_deny(&evaluate("./scripts/\"a b\"/python3 -c \"import os\"", &p)));
        assert!(is_deny(&evaluate("./scripts/\"a b\"/rm -rf x", &p)));

        let p = policy(&["cargo test*"], &[]);
        // Glob de `allowed` casa o texto bruto: aspas em volta da 1ª palavra não viram Allow (fail-closed).
        assert_eq!(evaluate("\"cargo\" test", &p), Decision::NeedsApproval);

        let p = policy(&["echo *"], &["rm -rf*"]);
        assert_eq!(evaluate("echo \"a b\"", &p), Decision::Allow);
    }

    #[test]
    fn regressao_limite_de_tamanho() {
        let p = policy(&["echo *"], &[]);
        let longo = format!("echo {}", "a".repeat(5000));
        assert!(is_deny(&evaluate(&longo, &p)));
    }

    #[test]
    fn regressao_curingas_entre_aspas_e_argumentos_seguros() {
        let p = policy(&["echo *", "cargo test*"], &["rm -rf*"]);
        assert_eq!(evaluate("echo \"a*b\"", &p), Decision::Allow);
        assert_eq!(evaluate("echo 'a*b'", &p), Decision::Allow);
        assert_eq!(evaluate("cargo test --lib", &p), Decision::Allow);
        assert_eq!(evaluate("cargo test -- --nocapture", &p), Decision::Allow);
    }

    #[test]
    fn regressao_f04_normaliza_padroes() {
        let p = policy(&["git  push*"], &[]);
        assert_eq!(evaluate("git push origin", &p), Decision::Allow);
        let p = policy(&["git  push*"], &["git  push  --force*"]);
        assert!(is_deny(&evaluate("git push --force origin", &p)));
    }

    #[test]
    fn always_allowable_accepts_plain_literal_commands() {
        assert!(is_always_allowable("cargo test --lib"));
        assert!(is_always_allowable("  git   log  -n1 "));
        assert!(is_always_allowable("ls -la --color=never ./src/a_b.c:1,@%+~"));
    }

    #[test]
    fn always_allowable_rejects_wrappers_interpreters_and_unsafe_text() {
        for cmd in [
            "sh -c x",
            "/bin/sh -c x",
            "env ls",
            "python3 -c x",
            "awk x",
            "find . -exec ls",
            "FOO=1 ls",
            "cargo test; rm x",
            "echo $HOME",
            "echo 'a b'",
            "echo a*",
            "rm ~/x",
            "echo ***",
            "",
            "   ",
        ] {
            assert!(!is_always_allowable(cmd), "não deveria ser persistível: {cmd}");
        }
    }

    #[test]
    fn from_settings() {
        let settings = Settings {
            base_branch: "main".into(),
            merge_strategy: "squash".into(),
            test_command: "cargo test".into(),
            allowed_commands: vec!["cargo test*".into()],
            denied_commands: vec!["sudo *".into()],
            checks: Vec::new(),
            formatters: Vec::new(),
            failure_artifacts: Vec::new(),
            screenshots: Vec::new(),
            environment: Default::default(),
            environment_hash: None,
            approved_environment_hashes: Vec::new(),
            max_attempts: crate::sdd::DEFAULT_LIMIT,
            max_test_cycles: crate::sdd::DEFAULT_LIMIT,
            yolo: false,
            review_diff_budget: None,
            skills: true,
            ask_architect_limit: None,
            architect_internet: false,
        };
        let p = Policy::from_settings(&settings);
        assert_eq!(p, policy(&["cargo test*"], &["sudo *"]));
        assert_eq!(evaluate("cargo test", &p), Decision::Allow);
        assert!(is_deny(&evaluate("sudo ls", &p)));
    }

    #[test]
    fn yolo_container_rules() {
        for cmd in [
            "mix deps.update cowlib", "mix test", "ls lib/*.ex", "find lib -name x", "git status", "rm -rf _build",
            "grep -F \"target/\" .gitignore", "python -c 'print(1)'", "find . -name x", "rm -rf build", "cat /etc/passwd",
            "ls -A | xargs rm -rf", "rm -rf $X", "cat .git/config",
        ] {
            assert!(yolo_container_excluded_reason(cmd).is_none(), "{cmd}");
        }
        for cmd in ["sudo mix", "sudo x", "docker ps", "mix run && sudo ls", "kubectl get pods", "podman run x", "echo x | su", "doas ls", ""] {
            assert!(yolo_container_excluded_reason(cmd).is_some(), "{cmd}");
        }
    }
}
