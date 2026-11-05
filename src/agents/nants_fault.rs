//! Spec 141 T03 (C-04): falha de testes causada pelo ambiente do Nants, reconhecida pela saída, sem
//! modelo. Caso real: o `node --test` sem caminho varria `/work/.nants-home` (o HOME do container até
//! a 141), tropeçava num link quebrado do PulseAudio e saía com erro antes de rodar teste. O
//! Desenvolvedor e a recuperação não podem consertar isso; gastar ciclos com eles é desperdício.
//!
//! **Regra** ([`detect`]), sobre a cauda ([`TAIL_LINES`] linhas não vazias) da saída do próprio
//! comando, já redigida, sem as notas do Nants nem os artefatos:
//! - **caminho do Nants dentro da árvore** ([`tree_paths`]: a pasta pessoal antiga, as íntegras
//!   `.nants-outputs`, o `.nants/` só leitura e as worktrees das tarefas), citado como caminho
//!   (absoluto em `/work`, ou relativo no começo de um trecho): basta. O projeto não cria nem lê essas
//!   pastas (ficam fora do Git pelo `.git/info/exclude`); um executor de testes que falha citando uma
//!   delas tropeçou no Nants;
//! - **a montagem do HOME** ([`home::CONTAINER_HOME`]): é também onde as ferramentas do projeto
//!   instalam (caches, navegadores), então só conta com erro de acesso na mesma linha
//!   ([`ACCESS_ERRORS`]: permissão, sistema só leitura, laço de links) E a mesma assinatura de falha
//!   do ciclo anterior (o Desenvolvedor passou por ela e nada mudou).
//!
//! **Falso positivo evitado:**
//! - o comando que roda sem falhar nunca chega aqui (só falha conta), nem o tempo limite nem o
//!   programa ausente (126/127, que já seguem como ambiente);
//! - linha de log antiga, fora da cauda, não conta;
//! - o caminho como pedaço de outro (`foo.nants/`, `/src/.nants-outputs`) não conta;
//! - se o próprio comando de testes cita um caminho do Nants, a configuração do projeto aponta para
//!   lá: a falha segue o caminho de sempre (a recuperação pode corrigir o comando).

use crate::agents::spill::OUTPUTS_DIR;
use crate::harness::compose::WORKDIR;
use crate::harness::home::{CONTAINER_HOME, LEGACY_HOME_DIR, TASK_WORKTREES};
use crate::sdd::environment::ENV_DIR;

/// Linhas não vazias do fim da saída em que o caminho precisa aparecer.
pub const TAIL_LINES: usize = 20;

/// Erros de acesso que, na linha que cita a montagem do HOME, apontam o ambiente (em minúsculas).
pub const ACCESS_ERRORS: [&str; 7] = [
    "permission denied",
    "eacces",
    "eperm",
    "operation not permitted",
    "read-only file system",
    "erofs",
    "too many levels of symbolic links",
];

/// As pastas do Nants dentro da árvore do projeto, relativas a ela.
pub fn tree_paths() -> [&'static str; 4] {
    [LEGACY_HOME_DIR, OUTPUTS_DIR, ENV_DIR, TASK_WORKTREES]
}

/// Fim de um trecho de caminho (o mesmo conjunto da assinatura de falhas) e o `=` de `chave=valor`.
fn delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ':' | ';' | '=')
}

/// `rest` começa pelo caminho `path` inteiro: depois dele vem `/`, um delimitador ou o fim.
fn starts_with_path(rest: &str, path: &str) -> bool {
    rest.strip_prefix(path).is_some_and(|after| after.chars().next().is_none_or(|c| c == '/' || delimiter(c)))
}

/// Começos de trecho da linha: o início e cada posição depois de um delimitador.
fn token_starts(line: &str) -> impl Iterator<Item = &str> {
    std::iter::once(line).chain(line.char_indices().filter(|(_, c)| delimiter(*c)).map(move |(i, c)| &line[i + c.len_utf8()..]))
}

/// O caminho do Nants dentro da árvore que a linha cita, na forma absoluta (`/work/<pasta>`).
fn tree_path_in(line: &str) -> Option<String> {
    token_starts(line).find_map(|rest| {
        tree_paths().into_iter().find_map(|name| {
            let absolute = format!("{WORKDIR}/{name}");
            let relative = format!("./{name}");
            [absolute.as_str(), relative.as_str(), name]
                .iter()
                .any(|form| starts_with_path(rest, form))
                .then_some(absolute)
        })
    })
}

/// A linha cita a montagem do HOME.
fn cites_home(line: &str) -> bool {
    token_starts(line).any(|rest| starts_with_path(rest, CONTAINER_HOME))
}

/// Linha com erro de acesso.
fn access_error(line: &str) -> bool {
    let lower = line.to_lowercase();
    ACCESS_ERRORS.iter().any(|e| lower.contains(e))
}

/// Linhas não vazias do fim de `output`.
fn tail_lines(output: &str) -> Vec<&str> {
    let lines: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let from = lines.len().saturating_sub(TAIL_LINES);
    lines[from..].to_vec()
}

/// O caminho do Nants que `text` cita em qualquer linha (dentro da árvore ou o HOME).
fn mentions_nants_path(text: &str) -> bool {
    text.lines().any(|l| tree_path_in(l).is_some() || cites_home(l))
}

/// Spec 141 T03: o caminho do Nants (constante, na forma absoluta do container) que explica a falha
/// de `command`, pela regra do topo do módulo. `output` é a saída do próprio comando, redigida;
/// `repeated` diz se a assinatura é a mesma do ciclo anterior. `None`: falha comum.
pub fn detect(output: &str, command: &str, repeated: bool) -> Option<String> {
    if mentions_nants_path(command) {
        return None;
    }
    let tail = tail_lines(output);
    if let Some(path) = tail.iter().rev().find_map(|l| tree_path_in(l)) {
        return Some(path);
    }
    (repeated && tail.iter().any(|l| cites_home(l) && access_error(l))).then(|| CONTAINER_HOME.to_string())
}

/// Marcador estrutural, na evidência gravada, do caminho que a detecção achou (revisão 1 da T03): a
/// nota da etapa e os artefatos entram depois, e a cauda da evidência pode já não citar o caminho.
pub const PATH_MARK: &str = "[caminho do Nants citado: ";

/// A linha do [`PATH_MARK`] com o caminho detectado.
pub fn path_mark(path: &str) -> String {
    format!("{PATH_MARK}{path}]\n")
}

/// O caminho de uma linha do [`PATH_MARK`], só se for um dos caminhos do Nants (nunca texto livre).
fn marked_path(line: &str) -> Option<String> {
    let path = line.trim().strip_prefix(PATH_MARK)?.strip_suffix(']')?;
    let known = path == CONTAINER_HOME || tree_paths().iter().any(|name| path == format!("{WORKDIR}/{name}"));
    known.then(|| path.to_string())
}

/// O caminho do Nants de uma evidência já gravada, para mostrar à pessoa qual caminho travou os
/// testes: o do [`PATH_MARK`] gravado na detecção; sem ele (evidência antiga), o citado na cauda.
pub fn cited(evidence: &str) -> Option<String> {
    if let Some(path) = evidence.lines().find_map(marked_path) {
        return Some(path);
    }
    let tail = tail_lines(evidence);
    tail.iter()
        .rev()
        .find_map(|l| tree_path_in(l))
        .or_else(|| tail.iter().any(|l| cites_home(l)).then(|| CONTAINER_HOME.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O caso real da T07 do Mini Street Fighter: uma linha só, citando a pasta pessoal antiga.
    #[test]
    fn the_real_case_cites_the_legacy_home() {
        let out = "Could not find '/work/.nants-home/.config/pulse/f9c892653055442f805c6ea95be61c10-runtime'\n";
        assert_eq!(detect(out, "node --test", false).as_deref(), Some("/work/.nants-home"));
    }

    #[test]
    fn every_tree_path_counts_in_absolute_and_relative_form() {
        for (line, want) in [
            ("Error: EROFS: read-only file system, open '/work/.nants/compose.yaml'", "/work/.nants"),
            ("ERROR collecting .nants-outputs/s1/3.txt", "/work/.nants-outputs"),
            ("SyntaxError in ./.sdd/worktrees/s1/T01/test/a.test.js", "/work/.sdd/worktrees"),
            ("cannot read .nants-home", "/work/.nants-home"),
            ("path=/work/.nants-outputs/x", "/work/.nants-outputs"),
        ] {
            let out = format!("TAP version 13\n{line}\n# fail 1\n");
            assert_eq!(detect(&out, "npm test", false).as_deref(), Some(want), "{line}");
        }
    }

    /// Falso positivo: pedaço de outro caminho, linha fora da cauda, comando que cita a pasta.
    #[test]
    fn ordinary_failures_are_not_nants_faults() {
        for line in [
            "AssertionError: expected 2 to equal 3",
            "open /work/src/.nants-outputs-like/x: no such file",
            "loaded foo.nants/config",
            "see /home/me/project/.nants-home/x",
            "/work/.nants-homework/x failed",
        ] {
            assert_eq!(detect(&format!("{line}\n"), "npm test", true), None, "{line}");
        }
        let old_log = format!("cache em /work/.nants-outputs/s/1.txt\n{}AssertionError\n", "ok\n".repeat(TAIL_LINES));
        assert_eq!(detect(&old_log, "npm test", true), None, "fora da cauda");
        let own = "Error: cannot open /work/.nants/compose.yaml\n";
        assert_eq!(detect(own, "docker compose -f .nants/compose.yaml run test", false), None, "o comando aponta para lá");
    }

    /// A montagem do HOME só conta com erro de acesso na mesma linha e a falha repetida.
    #[test]
    fn the_home_mount_needs_an_access_error_and_a_repeated_failure() {
        let denied = "npm ERR! Error: EACCES: permission denied, mkdir '/nants/home/.npm/_cacache'\n";
        assert_eq!(detect(denied, "npm test", false), None, "primeira vez");
        assert_eq!(detect(denied, "npm test", true).as_deref(), Some(CONTAINER_HOME));
        let missing = "browserType.launch: Executable doesn't exist at /nants/home/.cache/ms-playwright/chromium-1/chrome\n";
        assert_eq!(detect(missing, "npx playwright test", true), None, "o navegador do projeto não instalado");
        let log = "npm cache: /nants/home/.npm\nAssertionError: 1 != 2\n";
        assert_eq!(detect(log, "npm test", true), None, "log normal");
    }

    #[test]
    fn cited_reads_the_path_back_from_the_evidence() {
        let evidence = "[falha dos testes: código 1]\nCould not find '/work/.nants-home/.config/pulse/x'\n\nEtapa que falhou: 1 de 1\n";
        assert_eq!(cited(evidence).as_deref(), Some("/work/.nants-home"));
        assert_eq!(cited("[falha dos testes: código 1]\nAssertionError\n"), None);
    }

    /// Revisão 1 (T03): o caminho detectado vale mesmo quando a citação saiu da cauda (a nota da
    /// etapa entra depois da detecção); marcador com caminho que não é do Nants é ignorado.
    #[test]
    fn cited_prefers_the_path_marked_at_detection() {
        let notes = "nota da etapa\n".repeat(TAIL_LINES);
        let evidence = format!(
            "[falha dos testes: código 1]\n{}Could not find '/work/.nants-outputs/s/1.txt'\n{notes}",
            path_mark("/work/.nants-outputs")
        );
        assert_eq!(cited(&evidence).as_deref(), Some("/work/.nants-outputs"));
        let without = format!("[falha dos testes: código 1]\nCould not find '/work/.nants-outputs/s/1.txt'\n{notes}/nants/home/x\n");
        assert_eq!(cited(&without).as_deref(), Some(CONTAINER_HOME), "sem o marcador, a cauda (o erro antigo)");
        let forged = format!("[falha dos testes: código 1]\n{}", path_mark("/etc/passwd"));
        assert_eq!(cited(&forged), None);
    }
}
