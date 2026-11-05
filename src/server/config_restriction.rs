//! Spec 141 T04 (C-03): o REVISE_CONFIG que RESTRINGE uma etapa do comando de testes.
//!
//! A guarda de sempre (R1, [`super::recovery`]) aceita o comando novo que ainda executa cada etapa
//! original palavra por palavra. A restrição (R2) é outra coisa: a etapa fica com as mesmas palavras
//! e ganha argumentos no fim (`node --test` vira `node --test test/`). Ela troca o conjunto de testes
//! que roda, então:
//! - nunca é aplicada sozinha, nem com a aprovação automática: a pessoa aprova vendo a troca;
//! - só vira opção com evidência do SERVIDOR no registro de rodadas
//!   ([`crate::sdd::test_levels::SuiteLedger::restriction_probe`]): o próprio servidor roda a etapa
//!   oficial e a restrita na worktree da tarefa, no ambiente do projeto; a oficial precisa falhar com
//!   a mesma assinatura da falha que bloqueou a tarefa e a restrita precisa passar, na mesma árvore;
//! - os argumentos acrescentados (R3) são só caminhos que existem na worktree E na branch base da
//!   tarefa (um caminho criado pela própria tarefa, com um teste de fachada, é recusado). Toda opção (`-x`,
//!   `--grep`, `-k`, `--test-skip-pattern`, `--testPathIgnorePatterns`, `--passWithNoTests`,
//!   `--bail 0`...) é recusada: o efeito de uma opção depende da ferramenta e o servidor não tem
//!   como provar que ela não tira testes; um caminho, ele confere (existe, e a rodada restrita rodou
//!   ao menos um teste quando o executor informa a contagem);
//! - uma restrição só (uma etapa) por proposta, que vale para o `test_command`, o
//!   `full_test_command` e as checagens juntos (R4): onde a etapa aparece, ela é trocada.
//!
//! Tudo aqui é puro (sem rede), exceto a conferência do caminho no disco e na base (`git cat-file`).

use std::path::Path;

use crate::sdd::capabilities;

/// Máximo de argumentos acrescentados a uma etapa.
pub const MAX_APPENDED: usize = 4;
/// Limite de bytes de cada saída resumida mostrada à pessoa.
pub const MAX_SUMMARY_BYTES: usize = 1536;
/// Linhas de falha comparadas na falha que bloqueou a tarefa (as mesmas do runner).
const BLOCKED_SIGNATURE_LINES: usize = 10;
/// Linhas de falha lidas da rodada do servidor (a evidência guardada é só uma cauda).
const PROBE_SIGNATURE_LINES: usize = 200;
/// Linhas do fim da saída que entram no resumo.
const SUMMARY_TAIL_LINES: usize = 12;

/// Uma etapa restringida: o texto original da etapa e as palavras acrescentadas no fim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepChange {
    /// Palavras da etapa original (sem aspas, espaços normalizados).
    pub words: Vec<String>,
    /// A etapa original como está no comando (aspas preservadas).
    pub original: String,
    /// Argumentos acrescentados no fim.
    pub appended: Vec<String>,
}

impl StepChange {
    /// A etapa restrita, como vai rodar: a original seguida dos argumentos acrescentados.
    pub fn restricted(&self) -> String {
        format!("{} {}", self.original, self.appended.join(" "))
    }
}

/// Casa as etapas de `new` com as de `original` (as duas já analisadas por
/// [`super::recovery::parse_steps`]): cada etapa original precisa aparecer inteira (como na guarda
/// de sempre) ou com argumentos acrescentados no fim. Devolve as etapas restringidas (vazio = só a
/// guarda de sempre) ou `None` se alguma etapa original sumiu ou mudou de outro jeito.
pub fn match_steps(new: &[Vec<String>], original: &[(Vec<String>, String)]) -> Option<Vec<StepChange>> {
    let mut used = vec![false; new.len()];
    let mut changes = Vec::new();
    for (words, text) in original {
        if let Some(i) = (0..new.len()).find(|&i| !used[i] && new[i] == *words) {
            used[i] = true;
            continue;
        }
        let i = (0..new.len()).find(|&i| !used[i] && new[i].len() > words.len() && new[i][..words.len()] == words[..])?;
        used[i] = true;
        changes.push(StepChange { words: words.clone(), original: text.clone(), appended: new[i][words.len()..].to_vec() });
    }
    Some(changes)
}

/// Onde conferir que o caminho acrescentado já existia antes da tarefa: o repositório do projeto e
/// a branch base de onde a worktree saiu.
#[derive(Debug, Clone, Copy)]
pub struct Base<'a> {
    pub repo: &'a Path,
    pub branch: &'a str,
}

/// R3: os argumentos acrescentados são só caminhos relativos que existem dentro da worktree da
/// tarefa (`worktree`) e na branch base (`base`, conferido por `git cat-file -e <base>:<caminho>`):
/// a tarefa não pode criar o caminho que restringe a própria bateria. Recusa opção (qualquer palavra
/// que começa com `-`), atribuição, caminho absoluto, `..`, curinga, caractere fora de
/// `[A-Za-z0-9._/@+-]` e caminho que não existe. O motivo vai ao Arquiteto (texto para o modelo).
pub fn check_appended(appended: &[String], worktree: Option<&Path>, base: Option<Base<'_>>) -> Result<(), String> {
    if appended.is_empty() || appended.len() > MAX_APPENDED {
        return Err(format!("a restrição acrescenta de 1 a {MAX_APPENDED} caminhos no fim da etapa"));
    }
    let Some(worktree) = worktree else {
        return Err("sem a worktree da tarefa para conferir o caminho acrescentado".to_string());
    };
    let base_dir = worktree.canonicalize().map_err(|_| "sem a worktree da tarefa para conferir o caminho acrescentado".to_string())?;
    for word in appended {
        if word.starts_with('-') {
            return Err(format!(
                "`{word}` é uma opção; opções acrescentadas podem tirar testes da bateria e são recusadas (só um caminho que existe)"
            ));
        }
        let safe = word.chars().all(|c| c.is_ascii_alphanumeric() || "._/@+-".contains(c));
        if !safe || word.starts_with('/') || word.split('/').any(|part| part == "..") {
            return Err(format!("`{word}` não é um caminho relativo simples dentro do projeto"));
        }
        let inside = base_dir.join(word).canonicalize().ok().is_some_and(|p| p.starts_with(&base_dir));
        if !inside {
            return Err(format!("o caminho `{word}` não existe na worktree da tarefa"));
        }
        let relative = word.trim_start_matches("./").trim_end_matches('/');
        if relative.is_empty() || relative == "." {
            return Err(format!("`{word}` não restringe nada: é a raiz do projeto"));
        }
        if !base.is_some_and(|b| exists_in_base(b, relative)) {
            return Err(format!(
                "o caminho `{word}` não existe na branch base da tarefa; caminho criado pela própria tarefa não restringe a bateria"
            ));
        }
    }
    Ok(())
}

/// O caminho existe na branch base (`git cat-file -e <base>:<caminho>` no repositório do projeto).
fn exists_in_base(base: Base<'_>, relative: &str) -> bool {
    if base.branch.trim().is_empty() || base.branch.starts_with('-') {
        return false;
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(base.repo)
        .args(["cat-file", "-e", &format!("{}:{relative}", base.branch.trim())])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Troca, em `command`, cada etapa com as palavras de `change` pela etapa restrita. `None` se o
/// comando não é analisável (então ele não contém a etapa) ou se nada mudou.
pub fn rewrite(command: &str, change: &StepChange) -> Option<String> {
    let texts = super::recovery::command_steps(command)?;
    let words = super::recovery::parse_steps(command)?;
    let mut changed = false;
    let steps: Vec<String> = texts
        .into_iter()
        .zip(words)
        .map(|(text, step)| {
            if step == change.words {
                changed = true;
                format!("{text} {}", change.appended.join(" "))
            } else {
                text
            }
        })
        .collect();
    changed.then(|| steps.join(" && "))
}

/// A saída da rodada que bloqueou a tarefa, sem o cabeçalho estruturado e sem as seções que o runner
/// acrescenta depois dela (checagens, artefatos, critérios).
pub fn blocked_output(evidence: &str) -> &str {
    let mut text = evidence;
    if text.trim_start().starts_with(crate::agents::runner::TEST_FAILURE_MARK) {
        text = text.trim_start().split_once('\n').map_or("", |(_, rest)| rest);
    }
    match text.find("\n\n## ") {
        Some(at) => &text[..at],
        None => text,
    }
}

/// Quantos testes a saída diz que rodaram, em qualquer stack que informe: `tests 12` (node), `Ran 5
/// tests` (unittest), `5 passed` (pytest, jest, vitest, cargo, playwright), `5 total`. "no tests
/// ran/found" conta como zero. `None` quando o executor não informa (então não há o que conferir).
pub fn test_count(output: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    let mut keep = |n: u64| best = Some(best.map_or(n, |b| b.max(n)));
    for line in output.lines() {
        let lower = line.to_lowercase();
        if ["no tests ran", "no tests found", "no test files found", "no tests to run", "no tests were found"].iter().any(|z| lower.contains(z)) {
            keep(0);
        }
        let words: Vec<&str> = lower
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|w| !w.is_empty())
            .collect();
        let number = |w: &str| w.parse::<u64>().ok();
        // `# tests 12` / `ℹ tests 12`: a palavra `tests` abre a linha.
        if let (Some(&"tests"), Some(n)) = (words.first(), words.get(1).and_then(|w| number(w))) {
            keep(n);
        }
        for (i, w) in words.iter().enumerate() {
            let next = words.get(i + 1).copied();
            let prev = i.checked_sub(1).and_then(|p| words.get(p)).copied();
            match (*w, prev.and_then(number), next) {
                ("passed" | "total", Some(n), _) => keep(n),
                ("ran", _, Some(n)) if number(n).is_some() && words.get(i + 2).is_some_and(|t| t.starts_with("test")) => {
                    keep(number(n).unwrap_or(0));
                }
                _ => {}
            }
        }
    }
    best
}

/// Quantos testes a saída diz que falharam: `fail 1` (node), `1 failed` (pytest, jest, vitest,
/// cargo, playwright). `None` quando o executor não informa.
pub fn fail_count(output: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    for line in output.lines() {
        let lower = line.to_lowercase();
        let words: Vec<&str> = lower
            .split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|w| !w.is_empty())
            .collect();
        let mut keep = |n: u64| best = Some(best.map_or(n, |b| b.max(n)));
        if let (Some(&"fail"), Some(n)) = (words.first(), words.get(1).and_then(|w| w.parse::<u64>().ok())) {
            keep(n);
        }
        for pair in words.windows(2) {
            if let (Ok(n), "failed") = (pair[0].parse::<u64>(), pair[1]) {
                keep(n);
            }
        }
    }
    best
}

/// O que a evidência do servidor informa, pela saída do projeto: quantos testes a etapa restrita
/// rodou e quantos a oficial rodou e quantos falharam nela (cada um ausente se o executor não informa).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub tests: Option<u64>,
    pub original_tests: Option<u64>,
    pub original_failed: Option<u64>,
}

/// Resumo de uma saída para a pessoa: as linhas de falha e as últimas linhas, até
/// [`MAX_SUMMARY_BYTES`]. O chamador redige os segredos.
pub fn summarize(output: &str) -> String {
    let mut lines = capabilities::failing_lines(output, BLOCKED_SIGNATURE_LINES);
    let tail: Vec<&str> = output.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
    for line in &tail[tail.len().saturating_sub(SUMMARY_TAIL_LINES)..] {
        let line = line.chars().take(200).collect::<String>();
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    crate::agents::architect::truncate_bytes(&lines.join("\n"), MAX_SUMMARY_BYTES)
}

/// R2: a evidência do servidor sustenta a restrição? A etapa oficial falhou na rodada do servidor
/// com as mesmas linhas de falha (sem as durações) da falha que bloqueou a tarefa (`blocked`, a
/// evidência gravada), e a restrita passou e, quando o executor informa a contagem, rodou ao menos
/// um teste. Devolve as contagens informadas ([`Counts`]). O
/// motivo da recusa vai ao Arquiteto (texto para o modelo).
pub fn judge(blocked: &str, original_passed: bool, original_output: &str, restricted_passed: bool, restricted_output: &str) -> Result<Counts, String> {
    if original_passed {
        return Err("a etapa oficial passou na rodada do servidor; não há falha que a restrição resolva".to_string());
    }
    if !restricted_passed {
        return Err("a etapa restrita também falhou na rodada do servidor".to_string());
    }
    let wanted = capabilities::failure_signature(blocked_output(blocked), BLOCKED_SIGNATURE_LINES);
    if wanted.is_empty() {
        return Err("a falha que bloqueou a tarefa não tem linhas de falha para comparar com a rodada do servidor".to_string());
    }
    let seen = capabilities::failure_signature(original_output, PROBE_SIGNATURE_LINES);
    if !wanted.iter().all(|line| seen.contains(line)) {
        return Err("a etapa oficial falhou de outro jeito na rodada do servidor (a assinatura da falha mudou)".to_string());
    }
    let count = test_count(restricted_output);
    if count == Some(0) {
        return Err("a etapa restrita não rodou nenhum teste".to_string());
    }
    Ok(Counts { tests: count, original_tests: test_count(original_output), original_failed: fail_count(original_output) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn steps(command: &str) -> Vec<Vec<String>> {
        super::super::recovery::parse_steps(command).expect("analisável")
    }

    fn original(command: &str) -> Vec<(Vec<String>, String)> {
        steps(command).into_iter().zip(super::super::recovery::command_steps(command).expect("etapas")).collect()
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-restriction-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("test")).expect("mkdir");
        fs::write(dir.join("test/a.test.js"), "x").expect("arquivo");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git").args(args).current_dir(&dir).status().expect("git").success();
            assert!(ok, "{args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["add", "test"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@e", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "base"]);
        dir
    }

    #[test]
    fn steps_match_whole_or_with_appended_words_only() {
        let same = match_steps(&steps("npm ci && node --test"), &original("node --test")).expect("contém");
        assert!(same.is_empty(), "guarda de sempre");
        let narrowed = match_steps(&steps("node --test test/"), &original("node --test")).expect("restrição");
        assert_eq!(narrowed.len(), 1);
        assert_eq!((narrowed[0].original.as_str(), narrowed[0].appended.clone()), ("node --test", vec!["test/".to_string()]));
        assert_eq!(narrowed[0].restricted(), "node --test test/");
        // Etapa removida, outra ferramenta ou palavra trocada: sem casamento.
        for bad in ["npx playwright test", "node test/", "node --check test/", "npx node --test"] {
            assert!(match_steps(&steps(bad), &original("node --test && npx playwright test")).is_none(), "{bad}");
        }
    }

    #[test]
    fn appended_words_must_be_existing_paths_inside_the_worktree() {
        let wt = temp("paths");
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        let base = Base { repo: &wt, branch: "main" };
        assert!(check_appended(&words(&["test/"]), Some(&wt), Some(base)).is_ok());
        assert!(check_appended(&words(&["test/a.test.js"]), Some(&wt), Some(base)).is_ok());
        // Criado na tarefa (existe na worktree, não na base): recusado.
        fs::create_dir_all(wt.join("test/fake")).expect("mkdir");
        fs::write(wt.join("test/fake/x.test.js"), "x").expect("arquivo");
        let fake = check_appended(&words(&["test/fake/"]), Some(&wt), Some(base)).err().unwrap_or_default();
        assert!(fake.contains("branch base"), "{fake}");
        assert!(check_appended(&words(&["test/"]), Some(&wt), None).is_err(), "sem a base");
        assert!(check_appended(&words(&["./"]), Some(&wt), Some(base)).is_err(), "a raiz");
        for bad in [
            &["--test-skip-pattern", "e2e"][..],
            &["--grep", "x"],
            &["-k", "x"],
            &["--bail", "0"],
            &["--testPathIgnorePatterns", "e2e"],
            &["--passWithNoTests"],
            &["-x"],
            &["nao-existe/"],
            &["../fora"],
            &["/etc"],
            &["test/*.js"],
            &["FOO=1"],
            &[],
        ] {
            assert!(check_appended(&words(bad), Some(&wt), Some(base)).is_err(), "{bad:?}");
        }
        assert!(check_appended(&words(&["test/"]), None, Some(base)).is_err(), "sem worktree");
        let _ = fs::remove_dir_all(&wt);
    }

    #[test]
    fn rewrite_changes_the_step_wherever_it_appears() {
        let change = match_steps(&steps("node --test test/"), &original("node --test")).expect("restrição").remove(0);
        assert_eq!(rewrite("node --test && npx playwright test", &change).as_deref(), Some("node --test test/ && npx playwright test"));
        assert_eq!(rewrite("npm run lint", &change), None);
    }

    #[test]
    fn test_count_reads_common_runners_and_zero() {
        assert_eq!(test_count("ℹ tests 12\nℹ pass 12\nℹ fail 0"), Some(12));
        assert_eq!(test_count("# tests 3\n# pass 3"), Some(3));
        assert_eq!(test_count("Tests:       5 passed, 5 total"), Some(5));
        assert_eq!(test_count("===== 7 passed in 0.12s ====="), Some(7));
        assert_eq!(test_count("Ran 4 tests in 0.001s\n\nOK"), Some(4));
        assert_eq!(test_count("ℹ tests 0\nℹ pass 0"), Some(0));
        assert_eq!(test_count("No tests found, exiting with code 1"), Some(0));
        assert_eq!(test_count("tudo certo"), None);
        assert_eq!(fail_count("# tests 9\n# fail 1"), Some(1));
        assert_eq!(fail_count("Tests:  2 failed, 5 passed, 7 total"), Some(2));
        assert_eq!(fail_count("tudo certo"), None);
    }

    #[test]
    fn judge_needs_the_same_failure_and_a_real_pass() {
        let blocked = "[falha dos testes: código 1]\nnot ok 1 - e2e/game.spec.js\n# fail 1\n\n## Checagens\n- build ok";
        let official = "not ok 1 - e2e/game.spec.js\n  duration_ms: 812\n# tests 9\n# fail 1";
        let narrowed = "ok 1 - test/a.test.js\n# tests 8\n# pass 8";
        assert_eq!(judge(blocked, false, official, true, narrowed), Ok(Counts { tests: Some(8), original_tests: Some(9), original_failed: Some(1) }));
        assert!(judge(blocked, true, official, true, narrowed).is_err(), "oficial passou");
        assert!(judge(blocked, false, official, false, narrowed).is_err(), "restrito falhou");
        assert!(judge(blocked, false, "not ok 1 - outro teste", true, narrowed).is_err(), "outra assinatura");
        assert!(judge("[falha dos testes: código 1]\nalgo deu errado", false, official, true, narrowed).is_err(), "sem linhas de falha");
        assert!(judge(blocked, false, official, true, "# tests 0\n# pass 0").is_err(), "zero testes");
    }

    #[test]
    fn summary_keeps_failures_and_the_tail_within_the_limit() {
        let output = format!("{}\nnot ok 1 - e2e/game.spec.js\nfim", "linha\n".repeat(500));
        let summary = summarize(&output);
        assert!(summary.contains("not ok 1 - e2e/game.spec.js") && summary.contains("fim"));
        assert!(summary.len() <= MAX_SUMMARY_BYTES + 8);
    }
}
