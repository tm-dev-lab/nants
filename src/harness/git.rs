//! Wrapper síncrono do binário `git` com diretório de trabalho explícito.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::GitError;

/// Informação de uma worktree retornada por `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    /// Branch sem o prefixo `refs/heads/`; `None` para HEAD destacado ou bare.
    pub branch: Option<String>,
    pub head: String,
}

/// Variáveis de ambiente que redirecionam o repositório alvo do `git`.
/// São removidas de todo processo para que o `cwd` explícito prevaleça.
/// Configuração local do projeto: alterações nela nunca contam como base suja nem entram em
/// squash (o commit de integração só carrega o que está staged).
pub const LOCAL_SETTINGS: &str = ".sdd/settings.yaml";

const SCRUBBED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
];

/// Operações Git sobre o repositório em `root`.
#[derive(Debug, Clone)]
pub struct Git {
    root: PathBuf,
    /// Variáveis extras para cada invocação (vazio em produção; usado para isolar a identidade em testes).
    env: Vec<(String, String)>,
}

/// Linhas adicionadas/removidas de um arquivo no diff; `None` = binário.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumStat {
    pub path: String,
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// Rejeita valores vazios ou que o `git` interpretaria como opção.
fn check_arg(name: &'static str, value: &str) -> Result<(), GitError> {
    if value.is_empty() || value.starts_with('-') {
        return Err(GitError::InvalidArg {
            name,
            value: value.to_string(),
        });
    }
    Ok(())
}

impl Git {
    /// Cria o wrapper para o repositório em `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            env: Vec::new(),
        }
    }

    /// Acrescenta uma variável de ambiente a todas as invocações deste wrapper.
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    /// Diretório raiz do repositório usado como `cwd`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `true` quando não há alterações nem arquivos não rastreados.
    pub fn is_clean(&self) -> Result<bool, GitError> {
        Ok(self.run(["status", "--porcelain"])?.is_empty())
    }

    /// Como [`Git::is_clean`], mas ignora caminhos sob `.sdd/` (rastreados ou não): o estado do
    /// SDD não conta como sujeira da base.
    pub fn is_clean_ignoring_sdd(&self) -> Result<bool, GitError> {
        let args: Vec<OsString> = ["status", "--porcelain", "-z", "--untracked-files=all"]
            .iter()
            .map(OsString::from)
            .collect();
        // Bytes crus: `run` apagaria o espaço inicial da primeira entrada.
        let out = String::from_utf8_lossy(&self.run_bytes(&args)?).into_owned();
        let in_sdd = |p: &str| p == ".sdd" || p.starts_with(".sdd/");
        let mut parts = out.split('\0').filter(|p| !p.is_empty());
        while let Some(entry) = parts.next() {
            let status = entry.get(..2).unwrap_or("");
            let path = entry.get(3..).unwrap_or("");
            // Renomeações/cópias trazem o caminho de origem como próximo campo.
            let origin_in_sdd = if status.contains(['R', 'C']) {
                parts.next().is_none_or(in_sdd)
            } else {
                true
            };
            if !(in_sdd(path) && origin_in_sdd) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Nome da branch atual.
    pub fn current_branch(&self) -> Result<String, GitError> {
        Ok(self.run(["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string())
    }

    /// Resolve `rev` para o SHA completo.
    pub fn rev_parse(&self, rev: &str) -> Result<String, GitError> {
        check_arg("rev", rev)?;
        Ok(self.run(["rev-parse", rev])?.trim().to_string())
    }

    /// Cria (ou redefine, com `-B`) a branch `branch` a partir de `base` em `path`.
    pub fn worktree_add(&self, branch: &str, path: &Path, base: &str) -> Result<(), GitError> {
        check_arg("branch", branch)?;
        check_arg("base", base)?;
        self.run([
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-B"),
            OsStr::new(branch),
            path.as_os_str(),
            OsStr::new(base),
        ])?;
        Ok(())
    }

    /// Remove a worktree em `path` à força.
    pub fn worktree_remove(&self, path: &Path) -> Result<(), GitError> {
        self.run([
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            path.as_os_str(),
        ])?;
        Ok(())
    }

    /// Cria a worktree em `path` a partir da branch existente `branch`, sem redefini-la (sem `-B`).
    pub fn worktree_attach(&self, branch: &str, path: &Path) -> Result<(), GitError> {
        check_arg("branch", branch)?;
        self.run([
            OsStr::new("worktree"),
            OsStr::new("add"),
            path.as_os_str(),
            OsStr::new(branch),
        ])?;
        Ok(())
    }

    /// Lista as worktrees do repositório.
    pub fn worktree_list(&self) -> Result<Vec<WorktreeInfo>, GitError> {
        let out = self.run(["worktree", "list", "--porcelain"])?;
        Ok(parse_worktree_porcelain(&out))
    }

    /// `true` quando `path` é uma worktree registrada deste repositório (`git worktree list`).
    /// Pasta órfã (desregistrada, mas ainda no disco) devolve `false`.
    pub fn is_registered_worktree(&self, path: &Path) -> Result<bool, GitError> {
        let want = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(self.worktree_list()?.iter().any(|w| {
            std::fs::canonicalize(&w.path).unwrap_or_else(|_| w.path.clone()) == want
        }))
    }

    /// Nomes de todas as branches locais.
    pub fn branches(&self) -> Result<Vec<String>, GitError> {
        let out = self.run(["for-each-ref", "--format=%(refname:short)", "refs/heads"])?;
        Ok(out
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Indexa todas as alterações (`git add -A`): arquivos novos, modificados e removidos.
    pub fn add_all(&self) -> Result<(), GitError> {
        self.run(["add", "-A"])?;
        Ok(())
    }

    /// Diff `base...branch`: alterações da branch desde o ancestral comum com `base`.
    pub fn diff(&self, base: &str, branch: &str) -> Result<String, GitError> {
        check_arg("base", base)?;
        check_arg("branch", branch)?;
        let range = format!("{base}...{branch}");
        self.run(["diff", range.as_str()])
    }

    /// `git diff --stat` completo de `base...branch`.
    pub fn diff_stat(&self, base: &str, branch: &str) -> Result<String, GitError> {
        check_arg("base", base)?;
        check_arg("branch", branch)?;
        let range = format!("{base}...{branch}");
        self.run(["diff", "--stat", "--no-renames", range.as_str()])
    }

    /// `git diff --numstat` de `base...branch`: linhas +/- por arquivo (`None` = binário,
    /// detectado pelo próprio git).
    pub fn numstat(&self, base: &str, branch: &str) -> Result<Vec<NumStat>, GitError> {
        check_arg("base", base)?;
        check_arg("branch", branch)?;
        let range = format!("{base}...{branch}");
        let out = self.run(["diff", "--numstat", "--no-renames", "-z", range.as_str()])?;
        Ok(out
            .split('\0')
            .filter_map(|rec| {
                let mut it = rec.splitn(3, '\t');
                let (a, d, path) = (it.next()?, it.next()?, it.next()?);
                Some(NumStat {
                    path: path.to_string(),
                    added: a.parse().ok(),
                    deleted: d.parse().ok(),
                })
            })
            .collect())
    }

    /// Diff `base...branch` de um único arquivo.
    pub fn diff_file(&self, base: &str, branch: &str, path: &str) -> Result<String, GitError> {
        check_arg("base", base)?;
        check_arg("branch", branch)?;
        check_arg("path", path)?;
        let range = format!("{base}...{branch}");
        self.run(["--literal-pathspecs", "diff", "--no-renames", range.as_str(), "--", path])
    }

    /// Diff de um único arquivo: `base` contra o estado atual da worktree (inclui o que não foi
    /// commitado). Arquivo não rastreado não aparece.
    pub fn diff_file_worktree(&self, base: &str, path: &str) -> Result<String, GitError> {
        check_arg("base", base)?;
        check_arg("path", path)?;
        self.run(["--literal-pathspecs", "diff", "--no-renames", "--no-color", "--no-ext-diff", base, "--", path])
    }

    /// Caminhos alterados entre `base` e `branch` (`base...branch`), sem renomeação.
    pub fn changed_paths(&self, base: &str, branch: &str) -> Result<Vec<String>, GitError> {
        check_arg("base", base)?;
        check_arg("branch", branch)?;
        let range = format!("{base}...{branch}");
        let out = self.run(["diff", "--name-only", "--no-renames", "-z", range.as_str()])?;
        Ok(split_nul(&out))
    }

    /// Caminhos com alterações (staged, não staged ou não rastreadas), exceto dentro de
    /// `excluded_dirs`. Renomeações devolvem só o caminho novo.
    pub fn porcelain_paths(&self, excluded_dirs: &[&str]) -> Result<Vec<String>, GitError> {
        let mut args: Vec<String> = [
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--",
            ".",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for dir in excluded_dirs {
            check_arg("excluded", dir)?;
            args.push(format!(":(exclude){dir}"));
        }
        let out = self.run(args)?;
        let mut paths = Vec::new();
        let mut tokens = out.split('\0');
        while let Some(entry) = tokens.next() {
            if entry.len() < 4 {
                continue;
            }
            let status = &entry[..2];
            paths.push(entry[3..].to_string());
            // Renomeação/cópia: o token seguinte é o caminho de origem.
            if status.contains('R') || status.contains('C') {
                tokens.next();
            }
        }
        Ok(paths)
    }

    /// `true` quando há alterações já no índice (staged).
    pub fn has_staged_changes(&self) -> Result<bool, GitError> {
        Ok(!self.staged_paths()?.is_empty())
    }

    /// Caminhos com alterações no índice em relação a HEAD (inclui entradas não mescladas).
    pub fn staged_paths(&self) -> Result<Vec<String>, GitError> {
        let out = self.run(["diff", "--cached", "--no-renames", "--name-only", "-z", "HEAD"])?;
        Ok(split_nul(&out))
    }

    /// `true` se o índice tem exatamente a árvore de `rev`, ignorando o diretório `exclude`
    /// (`git diff --cached --quiet`). Qualquer erro além de "há diferenças" é propagado.
    pub fn staged_tree_equals(&self, rev: &str, exclude: &str) -> Result<bool, GitError> {
        check_arg("rev", rev)?;
        check_arg("exclude", exclude)?;
        let spec = format!(":(exclude){exclude}");
        match self.run(["diff", "--cached", "--quiet", rev, "--", ".", spec.as_str()]) {
            Ok(_) => Ok(true),
            Err(GitError::Failed { code: Some(1), .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Diretório Git próprio desta árvore (`git rev-parse --absolute-git-dir`): numa worktree
    /// ligada é `.git/worktrees/<nome>`, apagado junto com ela; na principal é o `.git`.
    pub fn private_git_dir(&self) -> Result<PathBuf, GitError> {
        Ok(PathBuf::from(self.run(["rev-parse", "--absolute-git-dir"])?.trim()))
    }

    /// Hash da árvore de trabalho como ela está agora (spec 124 T05): o `tree` que o Git gravaria
    /// com tudo o que está no disco, rastreado ou não, menos os caminhos ignorados pelo
    /// `.gitignore` e os diretórios de `exclude`. Nada muda no índice real nem no repositório
    /// além de objetos soltos: o cálculo usa uma cópia temporária do índice no diretório Git
    /// privado da árvore, apagada no fim.
    pub fn worktree_tree_hash(&self, exclude: &[&str]) -> Result<String, GitError> {
        for e in exclude {
            check_arg("exclude", e)?;
        }
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let git_dir = self.private_git_dir()?;
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let tmp = git_dir.join(format!("nants-tree-{}-{seq}.index", std::process::id()));
        // A cópia do índice real só acelera (o Git reaproveita o cache de `stat`); sem ela, começa vazio.
        let _ = std::fs::copy(git_dir.join("index"), &tmp);
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
                let _ = std::fs::remove_file(self.0.with_extension("index.lock"));
            }
        }
        let _cleanup = Cleanup(tmp.clone());
        let tmp_text = tmp.to_string_lossy().into_owned();
        let isolated = Git { root: self.root.clone(), env: self.env.clone() }.with_env("GIT_INDEX_FILE", &tmp_text);
        let mut add: Vec<String> = ["add", "-A", "--", "."].iter().map(|s| s.to_string()).collect();
        add.extend(exclude.iter().map(|e| format!(":(exclude){e}")));
        isolated.run(&add)?;
        let mut rm: Vec<String> = ["rm", "-r", "--cached", "-q", "--ignore-unmatch", "--"].iter().map(|s| s.to_string()).collect();
        rm.extend(exclude.iter().map(|e| e.to_string()));
        if !exclude.is_empty() {
            isolated.run(&rm)?;
        }
        Ok(isolated.run(["write-tree"])?.trim().to_string())
    }

    /// Estado do índice em relação a HEAD: pares `(status, caminho)` com status `A`, `M`, `D`, `T`
    /// ou `U`. Usado para capturar o resultado de um squash e para conferi-lo no rollback.
    pub fn staged_status(&self) -> Result<Vec<(String, String)>, GitError> {
        let out = self.run([
            "diff",
            "--cached",
            "--no-renames",
            "--name-status",
            "-z",
            "HEAD",
        ])?;
        let mut tokens = out.split('\0').filter(|t| !t.is_empty());
        let mut list = Vec::new();
        while let Some(status) = tokens.next() {
            let Some(path) = tokens.next() else { break };
            list.push((status.to_string(), path.to_string()));
        }
        Ok(list)
    }

    /// Indexa um caminho (`git add -- <path>`).
    pub fn add_path(&self, path: &str) -> Result<(), GitError> {
        check_arg("path", path)?;
        self.run(["add", "--", path])?;
        Ok(())
    }

    /// Desfaz um squash merge desta execução, sem `reset --hard`, usando somente `captured`.
    ///
    /// `captured` deve ser o resultado de [`Git::staged_status`] logo após o squash. Antes de agir,
    /// recusa (sem tocar em nada) se o índice diferir de `captured` ou se houver alteração na
    /// árvore fora de `.sdd/worktrees` e `.sdd/specs` cujo caminho não conste em `captured`.
    /// Depois, para cada entrada:
    /// - `M`, `D`, `T` e `U` cujo caminho existe em HEAD: restaurado para HEAD no índice e na
    ///   árvore;
    /// - `A` e `U` cujo caminho não existe em HEAD: removido do índice e do disco.
    ///
    /// Conferências antes de agir (F-12): nenhum caminho de `captured` pode ter alteração do humano.
    /// Isso inclui arquivo não rastreado (`??`) e alteração não staged (worktree diferente do
    /// índice) em qualquer caminho capturado; exceção: caminhos `U` (conflito do próprio squash, cujo
    /// worktree tem marcadores e portanto difere do índice). Para `A`, o conteúdo do worktree deve ser
    /// o blob do índice. Qualquer divergência devolve `Err("alteração do humano nos paths do squash;
    /// nada foi alterado")` antes de qualquer restore ou rm.
    ///
    /// Em erro durante a execução, parte dos caminhos pode já ter sido revertida: o chamador deve
    /// reportar o estado do índice.
    pub fn squash_rollback(&self, captured: &[(String, String)]) -> Result<(), GitError> {
        let fail = |stderr: String| GitError::Failed {
            command: "squash_rollback".to_string(),
            code: None,
            stderr,
        };
        // Sem paths capturados não há o que desfazer; `git status` sem pathspec varreria o repositório.
        if captured.is_empty() {
            return Ok(());
        }
        if self.staged_status()? != captured {
            return Err(fail("índice difere do squash capturado; nada foi alterado".into()));
        }
        let expected: HashSet<&str> = captured.iter().map(|(_, p)| p.as_str()).collect();
        for path in self.porcelain_paths(&[".sdd/worktrees", ".sdd/specs"])? {
            // `.sdd/settings.yaml` é configuração local: edição do humano nele não é do squash.
            // Se o squash o tocou, a conferência por caminho capturado abaixo cobre.
            // `.sdd/skills/` (perfis de especialista, spec 063) também é do humano/Arquiteto, não do squash.
            let skills = path.starts_with(".sdd/skills/");
            if path != LOCAL_SETTINGS && !skills && !expected.contains(path.as_str()) {
                return Err(fail(format!(
                    "alteração fora do squash em {path}; nada foi alterado"
                )));
            }
        }
        // Janela entre o squash e o rollback pode ser longa (test_command): o humano pode ter
        // editado um path do squash. Conferido antes de qualquer restore ou rm.
        let paths: Vec<&str> = captured.iter().map(|(_, p)| p.as_str()).collect();
        for (xy, path) in self.status_entries(&paths)? {
            let unmerged = captured.iter().any(|(s, p)| s == "U" && *p == path);
            let worktree_differs = xy.as_bytes().get(1).is_some_and(|b| *b != b' ');
            if xy == "??" || (!unmerged && worktree_differs) {
                return Err(fail(HUMAN_EDIT_MSG.to_string()));
            }
        }
        for (status, path) in captured {
            if status == "A" && self.worktree_blob(path)? != self.index_blob(path)? {
                return Err(fail(HUMAN_EDIT_MSG.to_string()));
            }
        }
        for (status, path) in captured {
            let (status, path) = (status.as_str(), path.as_str());
            let in_head = !self
                .run_literal(["ls-tree", "-r", "--name-only", "-z", "HEAD", "--", path])?
                .is_empty();
            match status {
                "A" | "U" if !in_head => {
                    self.run_literal(["rm", "-f", "--quiet", "--", path])?;
                }
                "M" | "D" | "T" | "U" => {
                    self.run_literal([
                        "restore",
                        "--source=HEAD",
                        "--staged",
                        "--worktree",
                        "--",
                        path,
                    ])?;
                }
                other => {
                    return Err(GitError::Failed {
                        command: "squash_rollback".to_string(),
                        code: None,
                        stderr: format!("status {other} inesperado em {path}"),
                    })
                }
            }
        }
        Ok(())
    }

    /// Entradas de `git status --porcelain` restritas a `paths`: pares `(XY, caminho)`.
    fn status_entries(&self, paths: &[&str]) -> Result<Vec<(String, String)>, GitError> {
        let mut args: Vec<String> = ["status", "--porcelain", "-z", "--untracked-files=all", "--"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.extend(paths.iter().map(|p| p.to_string()));
        let out = self.run_literal(args)?;
        let mut tokens = out.split('\0');
        let mut list = Vec::new();
        while let Some(entry) = tokens.next() {
            if entry.len() < 4 {
                continue;
            }
            let xy = entry[..2].to_string();
            let path = entry[3..].to_string();
            // Renomeação/cópia: o token seguinte é o caminho de origem.
            if xy.contains('R') || xy.contains('C') {
                tokens.next();
            }
            list.push((xy, path));
        }
        Ok(list)
    }

    /// Blob registrado no índice para `path` (vazio se ausente).
    fn index_blob(&self, path: &str) -> Result<String, GitError> {
        let out = self.run_literal(["ls-files", "-s", "--", path])?;
        Ok(out.split_whitespace().nth(1).unwrap_or_default().to_string())
    }

    /// Hash do conteúdo atual de `path` na árvore de trabalho.
    fn worktree_blob(&self, path: &str) -> Result<String, GitError> {
        Ok(self.run(["hash-object", "--", path])?.trim().to_string())
    }

    /// Assuntos dos commits de `rev` cuja mensagem contém `pattern` (texto literal), sem limite.
    pub fn subjects_matching(&self, rev: &str, pattern: &str) -> Result<Vec<String>, GitError> {
        check_arg("rev", rev)?;
        check_arg("pattern", pattern)?;
        let grep = format!("--grep={pattern}");
        let out = self.run([
            "log",
            "--fixed-strings",
            grep.as_str(),
            "--format=%s",
            rev,
        ])?;
        Ok(out.lines().map(str::to_string).collect())
    }

    /// Aplica `git merge --squash <branch>` na árvore atual, sem commitar.
    pub fn squash_merge(&self, branch: &str) -> Result<(), GitError> {
        check_arg("branch", branch)?;
        self.run(["merge", "--squash", branch])?;
        Ok(())
    }

    /// Cria um commit com a mensagem `msg` a partir do índice atual.
    pub fn commit(&self, msg: &str) -> Result<(), GitError> {
        self.run(["commit", "-m", msg])?;
        Ok(())
    }

    /// Indexa e commita somente `path` (add + commit restrito ao caminho). Falha desfaz o índice.
    pub fn commit_path(&self, path: &str, msg: &str) -> Result<(), GitError> {
        check_arg("path", path)?;
        self.run(["add", "--", path])?;
        let result = self.run(["commit", "-m", msg, "--", path]);
        if result.is_err() {
            let _ = self.run(["reset", "-q", "--", path]);
        }
        result.map(|_| ())
    }

    /// Indexa e commita somente `paths`. Sem mudança em relação ao HEAD não cria commit e devolve
    /// `false`. Falha no commit desfaz o índice desses caminhos.
    pub fn commit_paths(&self, paths: &[String], msg: &str) -> Result<bool, GitError> {
        for p in paths {
            check_arg("path", p)?;
        }
        let mut add = vec!["add".to_string(), "--".to_string()];
        add.extend(paths.iter().cloned());
        self.run(add)?;
        if !self.has_staged_changes()? {
            return Ok(false);
        }
        let mut commit = vec!["commit".to_string(), "-m".to_string(), msg.to_string(), "--".to_string()];
        commit.extend(paths.iter().cloned());
        let result = self.run(commit);
        if result.is_err() {
            let mut reset = vec!["reset".to_string(), "-q".to_string(), "--".to_string()];
            reset.extend(paths.iter().cloned());
            let _ = self.run(reset);
        }
        result.map(|_| true)
    }

    /// Arquivos rastreados na árvore de trabalho (`git ls-files`), ordenados pelo git.
    pub fn ls_files(&self) -> Result<Vec<String>, GitError> {
        let out = self.run(["ls-files", "-z"])?;
        Ok(split_nul(&out))
    }

    /// Rastreados + não rastreados e não ignorados (`ls-files -z --cached --others
    /// --exclude-standard`), sem repetição. Nomes que não são UTF-8 são descartados.
    pub fn ls_files_visible(&self) -> Result<Vec<String>, GitError> {
        let args: Vec<OsString> = ["ls-files", "-z", "--cached", "--others", "--exclude-standard"]
            .iter()
            .map(OsString::from)
            .collect();
        let out = self.run_bytes(&args)?;
        let mut paths: Vec<String> = out
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .filter_map(|p| String::from_utf8(p.to_vec()).ok())
            .collect();
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    /// `true` se `rel` deve ser tratado como ignorado pelo git (`check-ignore`). Falha fechada:
    /// só o exit code 1 significa "não ignorado"; 0, qualquer outro código ou erro de spawn
    /// `check-ignore` não expande pathspec (compara nomes) e pathspec mágico falha fechado, por isso
    /// não usa `--literal-pathspecs` (que o próprio `check-ignore` rejeita).
    /// contam como ignorado (quem usa isso para mascarar segredos mascara).
    pub fn is_ignored(&self, rel: &str) -> bool {
        let args: Vec<OsString> = ["check-ignore", "-q", "--", rel]
            .iter()
            .map(OsString::from)
            .collect();
        match self.command(&args).output() {
            Ok(o) => ignored_from_exit(o.status.code()),
            Err(_) => true,
        }
    }

    /// Caminho absoluto de um arquivo do gitdir real (`rev-parse --git-path`), que funciona
    /// também quando `.git` é um arquivo (worktree).
    pub fn git_path(&self, name: &str) -> Option<PathBuf> {
        let out = self.run(["rev-parse", "--git-path", name]).ok()?;
        let p = PathBuf::from(out.trim());
        Some(if p.is_absolute() { p } else { self.root.join(p) })
    }

    /// Conteúdo de `rel` no HEAD (`git show HEAD:rel`); `None` se ausente, sem HEAD ou inválido.
    pub fn show_head(&self, rel: &str) -> Option<String> {
        check_arg("path", rel).ok()?;
        self.run(["show".to_string(), format!("HEAD:{rel}")]).ok()
    }

    /// Número de arquivos rastreados em HEAD (0 em repositório sem commits).
    pub fn head_file_count(&self) -> usize {
        self.run(["ls-tree", "-r", "--name-only", "HEAD"])
            .map(|out| out.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0)
    }

    /// Commit de integração sem alterações no índice (`--allow-empty`), args fixos.
    pub fn commit_allow_empty(&self, msg: &str) -> Result<(), GitError> {
        self.run(["commit", "--allow-empty", "-m", msg])?;
        Ok(())
    }

    /// `true` quando HEAD é simbólico para uma branch sem commits (repositório recém-criado).
    pub fn is_unborn(&self) -> bool {
        self.rev_parse("HEAD").is_err() && self.run(["symbolic-ref", "-q", "HEAD"]).is_ok()
    }

    /// `true` quando o índice tem entradas. Em repositório sem commits, tudo que está no índice
    /// é staged (`git diff --cached HEAD` falha sem HEAD, por isso o uso de `ls-files`).
    pub fn index_has_entries(&self) -> Result<bool, GitError> {
        Ok(!self.run(["ls-files", "--cached", "-z"])?.is_empty())
    }

    /// `true` quando `rel` está versionado (no índice). Qualquer falha do Git conta como `false`.
    pub fn is_tracked(&self, rel: &str) -> bool {
        check_arg("path", rel).is_ok()
            && self
                .run(["--literal-pathspecs", "ls-files", "--cached", "-z", "--", rel])
                .is_ok_and(|out| !out.is_empty())
    }

    /// Chaves de identidade (`user.name`, `user.email`) sem valor configurado, em qualquer nível
    /// (local, global ou sistema). Cada chave é consultada separadamente.
    pub fn missing_identity(&self) -> Vec<&'static str> {
        ["user.name", "user.email"]
            .into_iter()
            .filter(|key| {
                self.run(["config", "--get", key])
                    .map_or(true, |value| value.trim().is_empty())
            })
            .collect()
    }

    /// Cria um commit vazio (`--allow-empty`) sem adicionar arquivos. É um commit do servidor, sem
    /// conteúdo a validar: `--no-verify` e `commit.gpgsign=false` evitam hooks do usuário e GPG.
    /// `fallback` são pares `chave, valor` passados com `-c` só nesta invocação (nunca altera a config).
    pub fn commit_empty(&self, msg: &str, fallback: &[(&str, &str)]) -> Result<(), GitError> {
        let mut args: Vec<String> = vec!["-c".into(), "commit.gpgsign=false".into()];
        for (key, value) in fallback {
            args.push("-c".into());
            args.push(format!("{key}={value}"));
        }
        for part in ["commit", "--allow-empty", "--no-verify", "-m", msg] {
            args.push(part.into());
        }
        self.run(args)?;
        Ok(())
    }

    /// Raiz do work tree que contém `root` (`rev-parse --show-toplevel`); `None` fora de repositório.
    pub fn toplevel(&self) -> Option<String> {
        self.run(["rev-parse", "--show-toplevel"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// `git init` com branch inicial `main` em `root`.
    pub fn init_main(&self) -> Result<(), GitError> {
        self.run(["init", "-q", "-b", "main"])?;
        Ok(())
    }

    /// Remove a branch `branch` à força (`git branch -D`).
    pub fn branch_delete(&self, branch: &str) -> Result<(), GitError> {
        check_arg("branch", branch)?;
        self.run(["branch", "-D", branch])?;
        Ok(())
    }

    /// Commits alcançáveis de `rev` cujo texto contém `pattern` (literal), do mais novo para o
    /// mais antigo. Só leitura. O filtro fino do assunto cabe a quem chama.
    pub fn log_grep(&self, rev: &str, pattern: &str) -> Result<Vec<CommitRecord>, GitError> {
        check_arg("rev", rev)?;
        check_arg("pattern", pattern)?;
        let grep = format!("--grep={pattern}");
        let args = [
            OsString::from("log"),
            OsString::from("--fixed-strings"),
            OsString::from(grep),
            OsString::from("--format=%H%x00%s%x00%ct"),
            OsString::from(rev),
        ];
        let out = self.run_bytes(&args)?;
        Ok(out
            .split(|b| *b == b'\n')
            .filter_map(|line| parse_commit_record(&String::from_utf8_lossy(line)))
            .collect())
    }

    /// Alterações de um commit (`diff-tree` na raiz, sem renomeação): pares (letra, caminho).
    /// Só leitura; o hash precisa ser hexadecimal.
    pub fn commit_changes(&self, hash: &str) -> Result<Vec<(char, String)>, GitError> {
        check_arg("hash", hash)?;
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GitError::InvalidArg {
                name: "hash",
                value: hash.to_string(),
            });
        }
        let args: Vec<OsString> = [
            "diff-tree",
            "--root",
            "--no-commit-id",
            "-r",
            "--no-renames",
            "--name-status",
            "-z",
            hash,
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let out = self.run_bytes(&args)?;
        // Nomes com quebra de linha, aspas ou bytes inválidos: `-z` preserva; bytes inválidos
        // viram U+FFFD em vez de derrubar a leitura.
        let fields: Vec<String> = out
            .split(|b| *b == 0)
            .filter(|chunk| !chunk.is_empty())
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect();
        Ok(fields
            .chunks(2)
            .filter_map(|pair| match pair {
                [status, path] => status.chars().next().map(|c| (c, path.clone())),
                _ => None,
            })
            .collect())
    }

    /// Assuntos (primeira linha) dos últimos `n` commits alcançáveis de `rev`.
    pub fn log_subjects(&self, rev: &str, n: usize) -> Result<Vec<String>, GitError> {
        check_arg("rev", rev)?;
        let count = format!("-n{n}");
        let out = self.run(["log", &count, "--format=%s", rev])?;
        Ok(out.lines().map(str::to_string).collect())
    }

    /// Como [`Git::run`], com `--literal-pathspecs`: `[`, `*`, `?` e `:` nos paths são texto literal,
    /// não glob nem magic pathspec. Usar em todo comando que recebe paths capturados.
    fn run_literal<I, S>(&self, args: I) -> Result<String, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut full: Vec<OsString> = vec![OsString::from("--literal-pathspecs")];
        full.extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self.run(full)
    }

    /// Executa `git <args>` em `root` e devolve stdout como texto.
    fn run<I, S>(&self, args: I) -> Result<String, GitError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|a| a.as_ref().to_os_string())
            .collect();
        let command = label(&args);
        let stdout = self.run_bytes(&args)?;
        String::from_utf8(stdout).map_err(|source| GitError::Utf8 { command, source })
    }

    /// Como [`Git::run`], mas devolve stdout como bytes crus (saídas que podem não ser UTF-8).
    fn run_bytes(&self, args: &[OsString]) -> Result<Vec<u8>, GitError> {
        let command = label(args);
        let output = self
            .command(args)
            .output()
            .map_err(|source| GitError::Spawn {
                command: command.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(GitError::Failed {
                command,
                code: output.status.code(),
                stderr: failure_text(&output.stderr, &output.stdout),
            });
        }
        Ok(output.stdout)
    }
}

/// Texto de falha: stderr; se vazio, stdout (ex.: "nothing to commit"), truncado em ~1 KiB.
fn failure_text(stderr: &[u8], stdout: &[u8]) -> String {
    let err = String::from_utf8_lossy(stderr).trim().to_string();
    if !err.is_empty() {
        return err;
    }
    let mut out = String::from_utf8_lossy(stdout).trim().to_string();
    if out.len() > 1024 {
        let mut cut = 1024;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('…');
    }
    out
}

impl Git {
    /// Monta o `Command` do `git` com `cwd` explícito e variáveis de redirecionamento removidas.
    fn command(&self, args: &[OsString]) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(&self.root);
        for var in SCRUBBED_ENV {
            cmd.env_remove(var);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        cmd
    }
}

/// Interpreta o exit code de `git check-ignore -q`: 1 = não ignorado; o resto = ignorado.
pub(crate) fn ignored_from_exit(code: Option<i32>) -> bool {
    code != Some(1)
}

/// Mensagem de recusa do rollback quando o humano alterou um path do squash.
const HUMAN_EDIT_MSG: &str = "alteração do humano nos paths do squash; nada foi alterado";

/// Commit de [`Git::log_grep`]: hash completo, assunto e instante (epoch, UTC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecord {
    pub hash: String,
    pub subject: String,
    pub committed_at: u64,
}

/// Uma linha de `git log --format=%H%x00%s%x00%ct`; linha malformada é descartada.
fn parse_commit_record(line: &str) -> Option<CommitRecord> {
    let mut parts = line.split('\0');
    let hash = parts.next()?;
    let subject = parts.next()?;
    let committed_at = parts.next()?.parse().ok()?;
    if hash.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(CommitRecord {
        hash: hash.to_string(),
        subject: subject.to_string(),
        committed_at,
    })
}

/// Separa uma saída `-z` em entradas não vazias.
fn split_nul(out: &str) -> Vec<String> {
    out.split('\0')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Representação legível do comando para mensagens de erro.
fn label(args: &[OsString]) -> String {
    let mut s = String::from("git");
    for a in args {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

/// Interpreta a saída de `git worktree list --porcelain`.
fn parse_worktree_porcelain(out: &str) -> Vec<WorktreeInfo> {
    let mut result = Vec::new();
    let mut current: Option<WorktreeInfo> = None;
    for line in out.lines() {
        if line.is_empty() {
            result.extend(current.take());
        } else if let Some(p) = line.strip_prefix("worktree ") {
            result.extend(current.take());
            current = Some(WorktreeInfo {
                path: PathBuf::from(p),
                branch: None,
                head: String::new(),
            });
        } else if let Some(h) = line.strip_prefix("HEAD ") {
            if let Some(w) = current.as_mut() {
                w.head = h.to_string();
            }
        } else if let Some(b) = line.strip_prefix("branch ") {
            if let Some(w) = current.as_mut() {
                w.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
            }
        }
    }
    result.extend(current);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;
    use std::process;

    /// Cria `<tmp>/nants-git-<pid>-<tag>/repo` com um commit inicial na branch `main`.
    /// Retorna `(diretório base, raiz do repo)`.
    fn init_repo(tag: &str) -> (PathBuf, PathBuf) {
        let base = env::temp_dir().join(format!("nants-git-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = Git::new(&repo);
        git.run(["init", "-b", "main"]).unwrap();
        git.run(["config", "user.name", "Nants Test"]).unwrap();
        git.run(["config", "user.email", "test@example.com"]).unwrap();
        git.run(["config", "commit.gpgsign", "false"]).unwrap();
        fs::write(repo.join("README.md"), "inicial\n").unwrap();
        git.run(["add", "."]).unwrap();
        git.commit("chore: commit inicial").unwrap();
        (base, repo)
    }

    #[test]
    fn path_queries_treat_magic_pathspecs_as_literal() {
        let (_base, repo) = init_repo("literal-pathspec");
        let git = Git::new(&repo);
        for magic in ["*", "**", ":(glob)**", ":(top)README.md", "READ*"] {
            assert!(!git.is_tracked(magic), "{magic}");
            assert!(git.diff_file_worktree("main", magic).unwrap().is_empty(), "{magic}");
        }
        fs::write(repo.join("README.md"), "mudou\n").unwrap();
        assert!(git.is_tracked("README.md"));
        assert!(git.diff_file_worktree("main", "README.md").unwrap().contains("+mudou"));
        assert!(git.diff_file_worktree("main", "*").unwrap().is_empty());
    }

    #[test]
    fn failure_message_uses_stdout_when_stderr_empty() {
        let (_base, repo) = init_repo("stdout-erro");
        let err = Git::new(&repo).commit("nada").unwrap_err().to_string();
        assert!(err.contains("nothing to commit"), "{err}");
        assert!(failure_text(b"", "é".repeat(2000).as_bytes()).len() <= 1030);
        assert_eq!(failure_text(b"erro", b"out"), "erro");
    }

    #[test]
    fn worktree_add_list_and_remove() {
        let (base, repo) = init_repo("worktree");
        let git = Git::new(&repo);
        let wt = base.join("wt-feat");

        git.worktree_add("sdd/s/t1", &wt, "main").unwrap();
        let list = git.worktree_list().unwrap();
        let entry = list
            .iter()
            .find(|w| fs::canonicalize(&w.path).ok() == fs::canonicalize(&wt).ok())
            .expect("worktree recém-criada deve aparecer na lista");
        assert_eq!(entry.branch.as_deref(), Some("sdd/s/t1"));
        assert_eq!(entry.head, git.rev_parse("main").unwrap());
        assert!(git.branches().unwrap().contains(&"sdd/s/t1".to_string()));
        assert!(list.iter().any(|w| w.branch.as_deref() == Some("main")));

        git.worktree_remove(&wt).unwrap();
        assert!(!wt.exists());
        let after = git.worktree_list().unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].branch.as_deref(), Some("main"));
        git.branch_delete("sdd/s/t1").unwrap();
        assert!(!git.branches().unwrap().contains(&"sdd/s/t1".to_string()));
    }

    #[test]
    fn squash_merge_then_commit() {
        let (base, repo) = init_repo("squash");
        let git = Git::new(&repo);
        let wt = base.join("wt-squash");

        git.worktree_add("feat", &wt, "main").unwrap();
        let wt_git = Git::new(&wt);
        fs::write(wt.join("feature.txt"), "x\n").unwrap();
        wt_git.run(["add", "."]).unwrap();
        wt_git.commit("feat: primeira").unwrap();
        fs::write(wt.join("feature2.txt"), "y\n").unwrap();
        wt_git.run(["add", "."]).unwrap();
        wt_git.commit("feat: segunda").unwrap();
        git.worktree_remove(&wt).unwrap();

        git.squash_merge("feat").unwrap();
        assert!(!git.is_clean().unwrap(), "squash deixa alterações staged");
        assert!(repo.join("feature.txt").exists());
        git.commit("feat(s): T01 - squash").unwrap();

        assert!(git.is_clean().unwrap());
        assert_eq!(
            git.log_subjects("HEAD", 2).unwrap(),
            vec!["feat(s): T01 - squash", "chore: commit inicial"]
        );
        git.branch_delete("feat").unwrap();
        assert!(!git.branches().unwrap().contains(&"feat".to_string()));
    }

    /// Spec 124 T05: o hash cobre o que está no disco (rastreado ou não), menos o ignorado e os
    /// diretórios excluídos; não mexe no índice real nem deixa o índice temporário.
    #[test]
    fn worktree_tree_hash_covers_the_disk_minus_ignored_and_excluded() {
        let (_base, repo) = init_repo("tree-hash");
        let git = Git::new(&repo);
        fs::write(repo.join(".gitignore"), "node_modules/\n").unwrap();
        git.run(["add", ".gitignore"]).unwrap();
        git.commit("chore: ignore").unwrap();
        let ex = [".sdd", ".nants-outputs"];
        let h0 = git.worktree_tree_hash(&ex).unwrap();
        assert_eq!(h0.len(), 40, "{h0}");
        // Ignorado e excluídos não mudam o hash.
        fs::create_dir_all(repo.join("node_modules/x")).unwrap();
        fs::write(repo.join("node_modules/x/i.js"), "1").unwrap();
        fs::create_dir_all(repo.join(".sdd/specs")).unwrap();
        fs::write(repo.join(".sdd/specs/t.md"), "estado").unwrap();
        fs::create_dir_all(repo.join(".nants-outputs/s")).unwrap();
        fs::write(repo.join(".nants-outputs/s/1.txt"), "saída").unwrap();
        assert_eq!(git.worktree_tree_hash(&ex).unwrap(), h0);
        // Arquivo novo não rastreado e edição de rastreado mudam; desfazer volta ao mesmo hash.
        fs::write(repo.join("novo.txt"), "a").unwrap();
        let h1 = git.worktree_tree_hash(&ex).unwrap();
        assert_ne!(h1, h0);
        fs::write(repo.join("README.md"), "mudou\n").unwrap();
        let h2 = git.worktree_tree_hash(&ex).unwrap();
        assert_ne!(h2, h1);
        fs::write(repo.join("README.md"), "inicial\n").unwrap();
        assert_eq!(git.worktree_tree_hash(&ex).unwrap(), h1);
        // Commitar o mesmo conteúdo não muda o hash (é o que o runner faz depois do Desenvolvedor).
        git.run(["add", "novo.txt"]).unwrap();
        git.commit("feat: novo").unwrap();
        assert_eq!(git.worktree_tree_hash(&ex).unwrap(), h1);
        // `.sdd` rastreado também fica fora.
        git.run(["add", "-f", ".sdd/specs/t.md"]).unwrap();
        git.commit("chore: sdd").unwrap();
        fs::write(repo.join(".sdd/specs/t.md"), "outro estado").unwrap();
        assert_eq!(git.worktree_tree_hash(&ex).unwrap(), h1);
        // O índice real segue intocado e o temporário não fica.
        assert_eq!(git.run(["status", "--porcelain", "--", "README.md", "novo.txt"]).unwrap(), "");
        let dir = git.private_git_dir().unwrap();
        assert!(!fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("nants-tree-")));
    }

    #[test]
    fn staged_tree_equals_ignores_excluded_dir_and_detects_differences() {
        let (base, repo) = init_repo("tree-equals");
        let git = Git::new(&repo);
        let wt = base.join("wt-tree");
        git.worktree_add("feat", &wt, "main").unwrap();
        let wt_git = Git::new(&wt);
        fs::write(wt.join("feature.txt"), "x\n").unwrap();
        wt_git.run(["add", "."]).unwrap();
        wt_git.commit("feat: primeira").unwrap();
        let sha = git.rev_parse("feat").unwrap();
        git.worktree_remove(&wt).unwrap();

        // A base andou só dentro de .sdd/: árvores iguais fora dele.
        fs::create_dir_all(repo.join(".sdd")).unwrap();
        fs::write(repo.join(".sdd/estado.md"), "a\n").unwrap();
        git.run(["add", "."]).unwrap();
        git.commit("sdd: estado").unwrap();
        git.squash_merge("feat").unwrap();
        assert!(git.staged_tree_equals(&sha, ".sdd").unwrap());
        assert!(!git.staged_tree_equals("main", ".sdd").unwrap());
        assert!(git.staged_tree_equals("rev-que-nao-existe", ".sdd").is_err());
        assert!(git.staged_tree_equals("-x", ".sdd").is_err());
        git.commit("feat: integra").unwrap();

        // A base andou fora de .sdd/: árvores diferentes.
        fs::write(repo.join("outro.txt"), "z\n").unwrap();
        git.run(["add", "."]).unwrap();
        git.commit("chore: outro").unwrap();
        git.run(["reset", "--soft", "HEAD"]).unwrap();
        git.run(["read-tree", "HEAD"]).unwrap();
        assert!(!git.staged_tree_equals(&sha, ".sdd").unwrap());
    }

    #[test]
    fn is_clean_detects_dirty_tree() {
        let (_base, repo) = init_repo("dirty");
        let git = Git::new(&repo);
        assert!(git.is_clean().unwrap());
        fs::write(repo.join("novo.txt"), "sujo\n").unwrap();
        assert!(!git.is_clean().unwrap());
    }

    #[test]
    fn current_branch_and_rev_parse() {
        let (_base, repo) = init_repo("branch");
        let git = Git::new(&repo);
        assert_eq!(git.current_branch().unwrap(), "main");
        let sha = git.rev_parse("HEAD").unwrap();
        assert_eq!(sha.len(), 40);
        assert_eq!(
            git.log_subjects("main", 5).unwrap(),
            vec!["chore: commit inicial"]
        );
    }

    #[test]
    fn failing_command_returns_failed_error() {
        let (_base, repo) = init_repo("failed");
        let git = Git::new(&repo);
        match git.rev_parse("rev-que-nao-existe") {
            Err(GitError::Failed {
                command,
                code,
                stderr,
            }) => {
                assert!(command.starts_with("git rev-parse"));
                assert!(code.is_some());
                assert!(!stderr.is_empty());
            }
            other => panic!("esperado GitError::Failed, veio {other:?}"),
        }
    }

    #[test]
    fn check_ignore_exit_codes_fail_closed() {
        assert!(ignored_from_exit(Some(0)));
        assert!(!ignored_from_exit(Some(1)));
        assert!(ignored_from_exit(Some(128)));
        assert!(ignored_from_exit(None));
    }

    #[test]
    fn command_removes_repository_redirect_env() {
        let git = Git::new("/tmp/qualquer");
        let cmd = git.command(&[OsString::from("status")]);
        let envs: Vec<_> = cmd.get_envs().collect();
        for var in SCRUBBED_ENV {
            assert!(
                envs.iter()
                    .any(|(k, v)| *k == OsStr::new(var) && v.is_none()),
                "{var} deve ser removida do ambiente do git"
            );
        }
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp/qualquer")));
    }

    #[test]
    fn option_like_revs_and_names_are_rejected_before_running() {
        let (base, repo) = init_repo("options");
        let git = Git::new(&repo);
        let target = base.join("saida.txt");
        let injected = format!("--output={}", target.display());

        assert!(matches!(
            git.log_subjects(&injected, 1),
            Err(GitError::InvalidArg { name: "rev", .. })
        ));
        assert!(matches!(
            git.rev_parse(&injected),
            Err(GitError::InvalidArg { name: "rev", .. })
        ));
        assert!(!target.exists(), "nenhum arquivo deve ser criado");

        assert!(matches!(
            git.rev_parse(""),
            Err(GitError::InvalidArg { name: "rev", .. })
        ));
        assert!(matches!(
            git.worktree_add("-x", &base.join("wt-x"), "main"),
            Err(GitError::InvalidArg { name: "branch", .. })
        ));
        assert!(matches!(
            git.worktree_add("feat", &base.join("wt-y"), "--detach"),
            Err(GitError::InvalidArg { name: "base", .. })
        ));
        assert!(matches!(
            git.worktree_add("", &base.join("wt-z"), "main"),
            Err(GitError::InvalidArg { name: "branch", .. })
        ));
        assert!(matches!(
            git.squash_merge("-q"),
            Err(GitError::InvalidArg { name: "branch", .. })
        ));
        assert!(matches!(
            git.branch_delete(""),
            Err(GitError::InvalidArg { name: "branch", .. })
        ));
        assert!(matches!(
            git.log_subjects("", 1),
            Err(GitError::InvalidArg { name: "rev", .. })
        ));
    }

    #[test]
    fn diff_attach_add_all_reset_and_clean_except() {
        let (base, repo) = init_repo("helpers");
        let git = Git::new(&repo);
        let wt = base.join("wt-helpers");
        git.worktree_add("feat-h", &wt, "main").unwrap();
        let wt_git = Git::new(&wt);
        fs::write(wt.join("novo.txt"), "x\n").unwrap();
        wt_git.add_all().unwrap();
        wt_git.commit("feat: novo").unwrap();
        assert!(git.diff("main", "feat-h").unwrap().contains("novo.txt"));
        assert!(git.diff("main", "main").unwrap().is_empty());

        fs::create_dir_all(repo.join(".sdd")).unwrap();
        fs::write(repo.join(".sdd/state.md"), "estado\n").unwrap();
        assert!(!git.is_clean().unwrap());
        assert_eq!(
            git.porcelain_paths(&[".sdd"]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            git.porcelain_paths(&[]).unwrap(),
            vec![".sdd/state.md".to_string()]
        );
        assert!(!git.has_staged_changes().unwrap());

        fs::remove_dir_all(repo.join(".sdd")).unwrap();

        fs::write(repo.join("README.md"), "alterado\n").unwrap();
        git.add_path("README.md").unwrap();
        assert!(git.has_staged_changes().unwrap());
        let captured = git.staged_status().unwrap();
        assert_eq!(captured, vec![("M".to_string(), "README.md".to_string())]);
        git.squash_rollback(&captured).unwrap();
        assert_eq!(fs::read_to_string(repo.join("README.md")).unwrap(), "inicial\n");
        assert!(!git.has_staged_changes().unwrap());

        // Squash que adiciona novo.txt: o rollback remove o arquivo.
        git.squash_merge("feat-h").unwrap();
        assert!(repo.join("novo.txt").exists());
        let captured = git.staged_status().unwrap();
        assert_eq!(captured, vec![("A".to_string(), "novo.txt".to_string())]);
        // Alteração fora da lista capturada: recusa e não toca em nada.
        fs::write(repo.join("extra.txt"), "x\n").unwrap();
        assert!(git.squash_rollback(&captured).is_err());
        assert!(repo.join("novo.txt").exists());
        assert!(git.has_staged_changes().unwrap());
        fs::remove_file(repo.join("extra.txt")).unwrap();
        git.squash_rollback(&captured).unwrap();
        assert!(!repo.join("novo.txt").exists(), "arquivo adicionado pelo squash removido");
        assert!(!git.has_staged_changes().unwrap());
        assert!(matches!(git.add_path("-x"), Err(GitError::InvalidArg { name: "path", .. })));

        git.worktree_remove(&wt).unwrap();
        git.worktree_attach("feat-h", &wt).unwrap();
        assert_eq!(Git::new(&wt).current_branch().unwrap(), "feat-h");
        assert!(matches!(
            git.worktree_attach("-x", &base.join("wt-bad")),
            Err(GitError::InvalidArg { name: "branch", .. })
        ));
        assert!(matches!(
            git.diff("--output=x", "main"),
            Err(GitError::InvalidArg { name: "base", .. })
        ));
        assert!(matches!(
            git.porcelain_paths(&[""]),
            Err(GitError::InvalidArg { name: "excluded", .. })
        ));
        assert!(matches!(
            git.subjects_matching("main", "-x"),
            Err(GitError::InvalidArg { name: "pattern", .. })
        ));
        assert!(matches!(
            git.add_path(""),
            Err(GitError::InvalidArg { name: "path", .. })
        ));
    }

    /// Cria a branch `name` a partir de main com um commit que grava `file` = `content`.
    fn branch_with(git: &Git, base: &Path, name: &str, file: &str, content: &str) {
        let wt = base.join(format!("wt-{name}"));
        git.worktree_add(name, &wt, "main").unwrap();
        fs::write(wt.join(file), content).unwrap();
        let wt_git = Git::new(&wt);
        wt_git.add_all().unwrap();
        wt_git.commit("feat: alteração").unwrap();
        git.worktree_remove(&wt).unwrap();
    }

    #[test]
    fn changed_paths_lists_branch_changes_against_base() {
        let (base, repo) = init_repo("changed-paths");
        let git = Git::new(&repo);
        fs::create_dir_all(repo.join(".sdd")).unwrap();
        fs::write(repo.join(".sdd/settings.yaml"), "a: 1\n").unwrap();
        git.run(["add", "."]).unwrap();
        git.commit("chore: settings").unwrap();
        branch_with(&git, &base, "feat-cp", ".sdd/settings.yaml", "outro\n");
        let paths = git.changed_paths("main", "feat-cp").unwrap();
        assert_eq!(paths, vec![".sdd/settings.yaml".to_string()]);
        assert!(git.changed_paths("main", "main").unwrap().is_empty());
        assert!(git.changed_paths("-x", "feat-cp").is_err(), "argumento com '-' é recusado");
    }

    #[test]
    fn rollback_tolerates_unstaged_local_settings_edit() {
        let (base, repo) = init_repo("rb-settings");
        let git = Git::new(&repo);
        fs::create_dir_all(repo.join(".sdd")).unwrap();
        fs::write(repo.join(".sdd/settings.yaml"), "a: 1\n").unwrap();
        git.run(["add", "."]).unwrap();
        git.commit("chore: settings").unwrap();
        branch_with(&git, &base, "feat-rs", "src.txt", "branch\n");
        git.squash_merge("feat-rs").unwrap();
        let captured = git.staged_status().unwrap();
        fs::write(repo.join(".sdd/settings.yaml"), "a: 2\n").unwrap();
        git.squash_rollback(&captured).expect("edição de settings não bloqueia o rollback");
        assert_eq!(fs::read_to_string(repo.join(".sdd/settings.yaml")).unwrap(), "a: 2\n");
        assert!(!repo.join("src.txt").exists(), "squash desfeito");
    }

    #[test]
    fn skill_profiles_do_not_interfere_with_squash_capture_or_rollback() {
        let (base, repo) = init_repo("rb-skills");
        let git = Git::new(&repo);
        fs::create_dir_all(repo.join(".sdd/skills")).unwrap();
        fs::write(repo.join(".sdd/skills/developer.md"), "a\n").unwrap();
        git.run(["add", "."]).unwrap();
        git.commit("docs: perfis").unwrap();
        branch_with(&git, &base, "feat-sk", "src.txt", "branch\n");
        git.squash_merge("feat-sk").unwrap();
        let captured = git.staged_status().unwrap();
        assert_eq!(captured, vec![("A".to_string(), "src.txt".to_string())], "perfil fora do squash");
        fs::write(repo.join(".sdd/skills/developer.md"), "editado pelo usuário\n").unwrap();
        git.squash_rollback(&captured).expect("edição de perfil não bloqueia o rollback");
        assert_eq!(fs::read_to_string(repo.join(".sdd/skills/developer.md")).unwrap(), "editado pelo usuário\n");
    }

    #[test]
    fn rollback_refuses_human_edit_on_modified_path() {
        let (base, repo) = init_repo("rb-mod");
        let git = Git::new(&repo);
        branch_with(&git, &base, "feat-m", "README.md", "branch\n");
        git.squash_merge("feat-m").unwrap();
        let captured = git.staged_status().unwrap();
        fs::write(repo.join("README.md"), "humano\n").unwrap();
        assert!(git.squash_rollback(&captured).is_err());
        assert_eq!(fs::read_to_string(repo.join("README.md")).unwrap(), "humano\n");
        assert_eq!(git.staged_status().unwrap(), captured, "índice intacto");
    }

    #[test]
    fn rollback_refuses_human_edit_on_added_path() {
        let (base, repo) = init_repo("rb-add");
        let git = Git::new(&repo);
        branch_with(&git, &base, "feat-a", "novo.txt", "branch\n");
        git.squash_merge("feat-a").unwrap();
        let captured = git.staged_status().unwrap();
        fs::write(repo.join("novo.txt"), "humano\n").unwrap();
        assert!(git.squash_rollback(&captured).is_err());
        assert_eq!(fs::read_to_string(repo.join("novo.txt")).unwrap(), "humano\n");
        assert_eq!(git.staged_status().unwrap(), captured, "índice intacto");
    }

    /// Grava `files` (caminho, conteúdo) na raiz do repositório e commita com `msg`.
    fn commit_files(git: &Git, repo: &Path, files: &[(&str, &str)], msg: &str) {
        for (path, content) in files {
            fs::write(repo.join(path), content).unwrap();
        }
        git.add_all().unwrap();
        git.commit(msg).unwrap();
    }

    /// Cria a branch `name` a partir de main com um commit que grava `files` e remove `removed`.
    fn branch_with_changes(
        git: &Git,
        base: &Path,
        name: &str,
        files: &[(&str, &str)],
        removed: &[&str],
    ) {
        let wt = base.join(format!("wt-{name}"));
        git.worktree_add(name, &wt, "main").unwrap();
        for (path, content) in files {
            fs::write(wt.join(path), content).unwrap();
        }
        for path in removed {
            fs::remove_file(wt.join(path)).unwrap();
        }
        let wt_git = Git::new(&wt);
        wt_git.add_all().unwrap();
        wt_git.commit("feat: alteração").unwrap();
        git.worktree_remove(&wt).unwrap();
    }

    #[test]
    fn rollback_handles_glob_like_names_literally() {
        let (base, repo) = init_repo("rb-glob");
        let git = Git::new(&repo);
        commit_files(
            &git,
            &repo,
            &[
                ("a[1].txt", "original\n"),
                ("a1.txt", "vizinho\n"),
                ("bx.txt", "vizinho b\n"),
            ],
            "chore: base",
        );
        branch_with_changes(
            &git,
            &base,
            "feat-g",
            &[("a[1].txt", "branch\n"), ("b*.txt", "novo\n")],
            &[],
        );
        git.squash_merge("feat-g").unwrap();
        let captured = git.staged_status().unwrap();
        assert_eq!(captured.len(), 2);

        // Vizinho alterado pelo humano fora do squash: recusa sem tocar em nada.
        fs::write(repo.join("a1.txt"), "humano\n").unwrap();
        assert!(git.squash_rollback(&captured).is_err());
        assert_eq!(fs::read_to_string(repo.join("a[1].txt")).unwrap(), "branch\n");
        assert_eq!(git.staged_status().unwrap(), captured, "índice intacto");

        fs::write(repo.join("a1.txt"), "vizinho\n").unwrap();
        git.squash_rollback(&captured).unwrap();
        assert_eq!(fs::read_to_string(repo.join("a[1].txt")).unwrap(), "original\n");
        assert!(!repo.join("b*.txt").exists(), "arquivo do squash removido");
        assert_eq!(fs::read_to_string(repo.join("bx.txt")).unwrap(), "vizinho b\n");
        assert_eq!(fs::read_to_string(repo.join("a1.txt")).unwrap(), "vizinho\n");
        assert!(!git.has_staged_changes().unwrap());
    }

    #[test]
    fn rollback_with_empty_capture_is_noop_despite_untracked_sdd_specs() {
        let (_base, repo) = init_repo("rb-empty");
        let git = Git::new(&repo);
        fs::create_dir_all(repo.join(".sdd/specs/003")).unwrap();
        fs::write(repo.join(".sdd/specs/003/spec.md"), "spec\n").unwrap();
        git.squash_rollback(&[]).unwrap();
        assert!(repo.join(".sdd/specs/003/spec.md").exists());
    }

    #[test]
    fn rollback_refuses_untracked_human_file_at_captured_path() {
        let (base, repo) = init_repo("rb-untracked");
        let git = Git::new(&repo);
        commit_files(&git, &repo, &[("velho.txt", "antigo\n")], "chore: velho");
        branch_with_changes(&git, &base, "feat-d", &[], &["velho.txt"]);
        git.squash_merge("feat-d").unwrap();
        let captured = git.staged_status().unwrap();
        assert_eq!(captured, vec![("D".to_string(), "velho.txt".to_string())]);

        fs::write(repo.join("velho.txt"), "humano\n").unwrap();
        assert!(git.squash_rollback(&captured).is_err());
        assert_eq!(fs::read_to_string(repo.join("velho.txt")).unwrap(), "humano\n");
        assert_eq!(git.staged_status().unwrap(), captured, "índice intacto");
    }

    #[test]
    fn rollback_restores_unmerged_path() {
        let (base, repo) = init_repo("rb-unmerged");
        let git = Git::new(&repo);
        branch_with_changes(&git, &base, "feat-u", &[("README.md", "branch\n")], &[]);
        commit_files(&git, &repo, &[("README.md", "main\n")], "chore: main avança");
        assert!(git.squash_merge("feat-u").is_err(), "conflito esperado no squash");
        let captured = git.staged_status().unwrap();
        assert_eq!(captured, vec![("U".to_string(), "README.md".to_string())]);

        git.squash_rollback(&captured).unwrap();
        assert_eq!(fs::read_to_string(repo.join("README.md")).unwrap(), "main\n");
        assert!(!git.has_staged_changes().unwrap());
        assert!(git.is_clean().unwrap());
    }

    #[test]
    fn parse_porcelain_handles_detached_and_bare() {
        let out = "worktree /a\nHEAD 111\nbranch refs/heads/main\n\nworktree /b\nHEAD 222\ndetached\n\nworktree /c\nbare\n";
        let parsed = parse_worktree_porcelain(out);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].branch.as_deref(), Some("main"));
        assert_eq!(parsed[1].branch, None);
        assert_eq!(parsed[1].head, "222");
        assert_eq!(parsed[2].branch, None);
        assert_eq!(parsed[2].head, "");
    }
}
