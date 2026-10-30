//! Comandos do guia de uso conferidos no ambiente (spec 137 T08, C-10).
//!
//! O README.md diz como executar e como testar o projeto. Os comandos dessas seções rodam dentro
//! da imagem do workspace (pelo Console, ou por `docker compose … workspace sh -lc '<cmd>'`), e um
//! comando que a imagem não tem só aparece para o usuário como `sh: 1: curl: not found`. Aqui o
//! Nants acha a primeira palavra de cada comando dessas seções ([`commands_from_readme`]), monta a
//! conferência com `command -v` ([`probe_script`]), que roda na sonda do workspace
//! (`ComposeEnv::probe`), e lê o resultado ([`parse_probe`]). O que sai vai ao Revisor da tarefa
//! que mexe no README como fato do ambiente.
//!
//! Genérico: nenhuma linguagem, imagem ou ferramenta é citada; o que conta é a primeira palavra.

use std::time::Duration;

/// Arquivo do guia de uso conferido, relativo à raiz da worktree.
pub const README_REL: &str = "README.md";
/// Tamanho máximo lido do README.
pub const README_MAX_BYTES: u64 = 256 * 1024;
/// Tempo máximo da conferência dentro do contêiner.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Prefixo das linhas da conferência, para achá-las no meio de qualquer saída do contêiner.
pub const PROBE_MARK: &str = "nants-cmd:";
/// Teto de comandos conferidos (os primeiros, na ordem do README).
pub const MAX_COMMANDS: usize = 24;
/// Teto do tamanho de um comando guardado como exemplo.
const MAX_EXAMPLE_CHARS: usize = 160;

/// Palavras (minúsculas) que marcam um título do README como "como executar" ou "como testar", em
/// pt e en (os títulos do relatório, "Como executar"/"Como testar" e "How to run"/"How to test",
/// estão entre eles).
const SECTION_WORDS: &[&str] = &["run", "test", "execut", "rodar", "start", "iniciar", "usage", "uso", "usar"];

/// Primeiras palavras que rodam no host, não na imagem: o comando de dentro vem do `sh -c`.
const HOST_WRAPPERS: &[&str] = &["docker", "docker-compose", "podman", "podman-compose"];

/// Rótulos de bloco de código só com comandos: toda linha conta.
const SCRIPT_FENCES: &[&str] = &["sh", "bash", "shell", "zsh"];

/// Rótulos de bloco que misturam comando e saída de exemplo (vazio = sem rótulo): só contam as
/// linhas com o prompt `$ ` ou que começam pelo wrapper do host ([`HOST_WRAPPERS`]).
const PROMPT_FENCES: &[&str] = &["", "console", "shell-session", "terminal"];

/// Como as linhas de um bloco de código contam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FenceKind {
    Script,
    Prompted,
    Other,
}

fn fence_kind(tag: &str) -> FenceKind {
    if SCRIPT_FENCES.contains(&tag) {
        FenceKind::Script
    } else if PROMPT_FENCES.contains(&tag) {
        FenceKind::Prompted
    } else {
        FenceKind::Other
    }
}

/// A linha começa pelo wrapper do host (`docker compose …`)?
fn starts_with_wrapper(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|w| HOST_WRAPPERS.contains(&w))
}

/// Num bloco [`FenceKind::Prompted`], a linha é comando (`$ cmd` ou o wrapper)? Devolve o comando.
fn prompted_command(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if let Some(cmd) = trimmed.strip_prefix("$ ") {
        return Some(cmd);
    }
    starts_with_wrapper(trimmed).then_some(trimmed)
}

/// Primeira palavra de um comando do guia e o comando onde ela aparece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageCommand {
    pub word: String,
    pub example: String,
}

/// Resultado da conferência de uma palavra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCheck {
    pub word: String,
    pub example: String,
    pub found: bool,
}

/// Nível e texto de um título Markdown (`## Título`), ou `None`.
fn heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim().to_lowercase()))
}

fn is_usage_title(title: &str) -> bool {
    SECTION_WORDS.iter().any(|w| title.contains(w))
}

/// Palavra aceitável como nome de comando: só letras, dígitos e `._+-/`, sem começar por `-`.
/// O conjunto também torna seguro pô-la entre aspas simples no script da sonda.
fn is_command_word(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= 64
        && !word.starts_with('-')
        && word.chars().any(|c| c.is_ascii_alphabetic())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '/'))
}

/// O comando de dentro de `docker compose … sh -lc '<cmd>'` (ou `bash -c "<cmd>"`). `None` sem ele.
fn inner_of_wrapper(line: &str) -> Option<String> {
    for shell in ["sh", "bash"] {
        for flag in ["-lc", "-c", "-l -c"] {
            let needle = format!(" {shell} {flag} ");
            if let Some(at) = line.find(&needle) {
                let rest = line[at + needle.len()..].trim();
                let quote = rest.chars().next()?;
                if quote == '\'' || quote == '"' {
                    let body = &rest[1..];
                    let end = body.rfind(quote).unwrap_or(body.len());
                    return Some(body[..end].to_string());
                }
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Primeiras palavras dos comandos de uma linha de shell (separados por `&&`, `||`, `;` e `|`).
/// Linha de host (docker/podman) só conta pelo comando de dentro; sem ele, não conta.
fn words_of_line(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix("$ ").unwrap_or(line).trim();
    if line.is_empty() || line.starts_with('#') {
        return Vec::new();
    }
    let first = line.split_whitespace().next().unwrap_or("");
    let body = if HOST_WRAPPERS.contains(&first) {
        match inner_of_wrapper(line) {
            Some(inner) => inner,
            None => return Vec::new(),
        }
    } else {
        line.to_string()
    };
    let normalized = body.replace("&&", "\n").replace("||", "\n").replace([';', '|'], "\n");
    normalized
        .lines()
        .filter_map(|part| {
            let mut words = part.split_whitespace().map(|w| w.trim_start_matches('('));
            // Atribuições de ambiente na frente (`FOO=1 cmd`) não são o comando.
            let word = words.find(|w| !(w.contains('=') && !w.starts_with('=')))?;
            is_command_word(word).then(|| word.to_string())
        })
        .collect()
}

fn example(line: &str) -> String {
    let line = line.trim();
    let line = line.strip_prefix("$ ").unwrap_or(line);
    if line.chars().count() <= MAX_EXAMPLE_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_EXAMPLE_CHARS).collect();
    format!("{cut}...")
}

/// Trechos de código em linha que são comandos sem dúvida: só os que começam pelo wrapper do host
/// (`` `docker compose … sh -lc '<cmd>'` ``), que contam pelo comando de dentro. Qualquer outro
/// código em linha (`GET /health`, `Ctrl + C`, um caminho) fica de fora.
fn inline_commands(line: &str) -> Vec<String> {
    line.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, span)| span.trim().to_string())
        .filter(|span| starts_with_wrapper(span))
        .collect()
}

/// A primeira palavra de cada comando das seções de executar e testar do README (títulos com
/// [`SECTION_WORDS`], com as subseções), na ordem e sem repetição, até [`MAX_COMMANDS`]. Entram:
/// toda linha dos blocos `sh`/`bash`/`shell`/`zsh`; nos blocos sem rótulo, `console`,
/// `shell-session` e `terminal`, só as linhas com o prompt `$ ` ou o wrapper do host; e o código em
/// linha que começa pelo wrapper. Linhas que terminam em `\` são juntadas antes. O que não dá para
/// dizer que é comando fica de fora: falso "ausente" reprovaria a tarefa sem motivo.
pub fn commands_from_readme(text: &str) -> Vec<UsageCommand> {
    let mut out: Vec<UsageCommand> = Vec::new();
    let mut active: Option<usize> = None;
    let mut fence: Option<(String, FenceKind)> = None;
    let mut pending = String::new();
    let push = |line: &str, out: &mut Vec<UsageCommand>| {
        for word in words_of_line(line) {
            if out.len() < MAX_COMMANDS && !out.iter().any(|c| c.word == word) {
                out.push(UsageCommand { word, example: example(line) });
            }
        }
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some((marker, kind)) = &fence {
            if trimmed.starts_with(marker.as_str()) {
                fence = None;
                pending.clear();
                continue;
            }
            // Continuação com `\`: junta à linha seguinte antes de extrair.
            let piece = line.trim_end();
            if let Some(head) = piece.strip_suffix('\\') {
                pending.push_str(head.trim());
                pending.push(' ');
                continue;
            }
            let full = format!("{pending}{}", piece.trim());
            pending.clear();
            if active.is_none() {
                continue;
            }
            match kind {
                FenceKind::Script => push(&full, &mut out),
                FenceKind::Prompted => {
                    if let Some(cmd) = prompted_command(&full) {
                        push(cmd, &mut out);
                    }
                }
                FenceKind::Other => {}
            }
            continue;
        }
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            let marker = trimmed[..3].to_string();
            let tag = trimmed[3..].trim().split_whitespace().next().unwrap_or("").to_lowercase();
            fence = Some((marker, fence_kind(&tag)));
            continue;
        }
        if let Some((level, title)) = heading(line) {
            if active.is_some_and(|l| level <= l) {
                active = None;
            }
            if active.is_none() && is_usage_title(&title) {
                active = Some(level);
            }
            continue;
        }
        if active.is_some() {
            for cmd in inline_commands(line) {
                push(&cmd, &mut out);
            }
        }
    }
    out
}

/// Script `sh` da conferência: `command -v` de cada palavra, uma linha marcada por palavra, dentro
/// de um shell de login (`sh -lc`, como o Console, os testes e o README: o login pode redefinir o
/// PATH em `/etc/profile`). Sai sempre com 0. As palavras passam por [`is_command_word`] (sem aspas,
/// espaço nem metacaractere), o que as deixa seguras dentro das aspas simples do `sh -lc`.
pub fn probe_script(commands: &[UsageCommand]) -> String {
    let words: Vec<&str> = commands.iter().map(|c| c.word.as_str()).filter(|w| is_command_word(w)).collect();
    format!(
        "exec sh -lc 'for w in {}; do if command -v \"$w\" >/dev/null 2>&1; then echo \"{PROBE_MARK}ok:$w\"; else echo \"{PROBE_MARK}missing:$w\"; fi; done; true'",
        words.join(" ")
    )
}

/// Lê a saída da sonda: cada comando com o resultado. Palavra sem linha na saída fica de fora
/// (não conferida não é ausente).
pub fn parse_probe(commands: &[UsageCommand], output: &str) -> Vec<CommandCheck> {
    let mut seen: Vec<(String, bool)> = Vec::new();
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix(PROBE_MARK) else { continue };
        let (state, word) = match rest.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let found = match state {
            "ok" => true,
            "missing" => false,
            _ => continue,
        };
        seen.push((word.to_string(), found));
    }
    commands
        .iter()
        .filter_map(|c| {
            let found = seen.iter().find(|(w, _)| *w == c.word)?.1;
            Some(CommandCheck { word: c.word.clone(), example: c.example.clone(), found })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        commands_from_readme(text).into_iter().map(|c| c.word).collect()
    }

    #[test]
    fn takes_first_words_from_run_and_test_sections_only() {
        let readme = "# App\n\n## Prerequisites\n```sh\ngit clone x\n```\n\n## How to run\n```bash\n./mvnw spring-boot:run\n```\n\n## How to test\n```console\n$ curl http://localhost:8080/health\nHTTP/1.1 200 OK\n```\n2. Open `src/main.rs`.\n\n```sh\n$ npm ci && npm test | tee out.txt\nFOO=1 make check\n# comentário\n```\n\n## License\n```sh\nlicensecheck .\n```\n";
        assert_eq!(words(readme), ["./mvnw", "curl", "npm", "tee", "make"]);
    }

    #[test]
    fn docker_wrapper_counts_by_the_inner_command_and_bare_host_lines_do_not_count() {
        let readme = "## Como executar\n```sh\ndocker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc 'cd app && java -jar app.jar'\ndocker compose -f .nants/compose.yaml up\n```\n### Como testar\n```\ndocker compose run --rm workspace bash -c \"pytest -q\"\n```\n";
        assert_eq!(words(readme), ["cd", "java", "pytest"]);
    }

    #[test]
    fn subsections_stay_and_a_sibling_heading_ends_the_section() {
        let readme = "## Como rodar\n### Backend\n```sh\nuvicorn app:main\n```\n## Arquitetura\n```sh\nhttpie x\n```\n";
        assert_eq!(words(readme), ["uvicorn"]);
    }

    #[test]
    fn non_shell_fences_and_odd_words_are_ignored() {
        let readme = "## How to test\n```json\n{\"curl\": 1}\n```\n```sh\n-v\n'quoted' x\n```\n`GET /health 200` and `http://x y`\n";
        assert!(words(readme).is_empty(), "{:?}", words(readme));
    }

    /// Revisão 1 da T08 (F-01): o que não dá para dizer que é comando fica de fora.
    #[test]
    fn inline_code_and_sample_output_are_not_commands() {
        let readme = "## How to test\n`GET /health` retorna 200. Pare com `Ctrl + C`.\nRode `npm test` no Console.\n\n```\nHTTP/1.1 200 OK\nTests run: 5\nOK\n```\n```console\nTests run: 5, Failures: 0\n```\n```shell-session\nOK\n```\n";
        assert!(words(readme).is_empty(), "{:?}", words(readme));
    }

    #[test]
    fn backslash_continuation_is_joined_and_never_yields_the_next_line_as_a_command() {
        let readme = "## How to run\n```sh\ndocker compose -f .nants/compose.yaml run --rm --service-ports \\\n  workspace sh -lc './gradlew bootRun'\n```\n```\n$ docker compose run --rm \\\n    workspace sh -lc 'pytest -q'\n200 OK\n```\n";
        assert_eq!(words(readme), ["./gradlew", "pytest"]);
        let joined = &commands_from_readme(readme)[0].example;
        assert!(joined.starts_with("docker compose -f .nants/compose.yaml run --rm --service-ports workspace sh -lc"), "{joined}");
    }

    #[test]
    fn prompted_lines_and_wrapped_inline_code_count() {
        let readme = "## How to test\n```console\n$ curl -s http://localhost:8080/health\n{\"status\":\"UP\"}\n```\nOu `docker compose -f .nants/compose.yaml run --rm workspace sh -lc 'wget -qO- x'`.\n";
        assert_eq!(words(readme), ["curl", "wget"]);
    }

    #[test]
    fn probe_script_runs_in_a_login_shell_and_parse_reads_only_marked_lines() {
        let cmds = commands_from_readme("## How to test\n```sh\ncurl -s x\nnpm test\n```\n");
        let script = probe_script(&cmds);
        assert!(script.starts_with("exec sh -lc 'for w in curl npm;") && script.ends_with("done; true'"), "{script}");
        assert!(script.contains("command -v \"$w\""), "{script}");
        // Uma única aspa simples abre e outra fecha o `sh -lc`: nenhuma palavra traz aspas.
        assert_eq!(script.matches('\'').count(), 2, "{script}");
        let odd = [UsageCommand { word: "x';rm".into(), example: String::new() }];
        assert!(!probe_script(&odd).contains("rm"), "palavra fora do conjunto seguro não entra");
        let out = "noise\nnants-cmd:missing:curl\nnants-cmd:ok:npm\nnants-cmd:weird:x\n";
        let checks = parse_probe(&cmds, out);
        assert_eq!(checks.len(), 2);
        assert!(!checks[0].found && checks[0].word == "curl" && checks[0].example == "curl -s x");
        assert!(checks[1].found);
        assert!(parse_probe(&cmds, "").is_empty(), "sem linha = não conferido");
    }
}
