//! Saída grande de ferramenta guardada em arquivo (spec 122 T02).
//!
//! Regra única, por tamanho, para todas as ferramentas: o resultado que passa da sua parte do
//! orçamento chega ao modelo com o começo (1/5) e o fim (4/5) dessa parte e, no meio, uma linha
//! dizendo que a saída foi cortada, com o caminho do arquivo que guarda a íntegra e o tamanho
//! total. Os resultados de uma mesma resposta dividem um orçamento total: os pequenos ficam
//! inteiros e os grandes dividem o restante.
//!
//! A íntegra fica em `.nants-outputs/<sessão>/<n>.txt` na raiz do agente (a worktree), onde o
//! `read_file` lê e, no docker, onde o contêiner também vê (pelo bind de `/work`). A pasta fica
//! fora de `.nants/` de propósito: lá é o contexto do docker build e a base do hash do ambiente
//! aprovado. É só do usuário, nunca entra no controle de versão (`.gitignore` próprio e a linha
//! no `info/exclude`) e sai com a worktree. Fora de uma worktree (o projeto do Arquiteto), a pasta
//! da sessão é apagada no fim da sessão. Tetos: [`MAX_FILE_BYTES`] por arquivo e
//! [`MAX_SESSION_BYTES`] por sessão; acima disso, só o corte.
//!
//! Valores iniciais declarados (calibrar com o evento `spill` do `context.jsonl`):
//! [`RESPONSE_BUDGET_BYTES`], o limite de cada resultado (`MAX_TOOL_RESULT_BYTES`) e
//! [`MIN_SHARE_BYTES`].

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::agents::usage::ContextChange;
use crate::harness::tools::MAX_TOOL_RESULT_BYTES;

/// Pasta das íntegras, na raiz do agente (fora de `.nants/`, ver o topo do módulo).
pub const OUTPUTS_DIR: &str = ".nants-outputs";

/// Orçamento total dos resultados de uma mesma resposta do modelo.
pub const RESPONSE_BUDGET_BYTES: usize = 48 * 1024;
/// Parte mínima de um resultado cortado (muitas chamadas grandes na mesma resposta).
pub const MIN_SHARE_BYTES: usize = 1024;
/// Teto de cada arquivo guardado: acima dele, só o corte.
pub const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Teto de disco da sessão: depois dele, só o corte.
pub const MAX_SESSION_BYTES: u64 = 500 * 1024 * 1024;
/// Início da linha do corte (o índice da compactação a reconhece como linha do harness).
pub const CUT_PREFIX: &str = "[saída cortada: ";
/// Trecho da linha do corte que precede o caminho da íntegra.
const PATH_MARK: &str = " linhas) está em ";
/// Tentativas de nome livre para a pasta da sessão.
const MAX_DIR_ATTEMPTS: u32 = 1000;

/// Um resultado de ferramenta da resposta: o nome da ferramenta e o texto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub tool: String,
    pub content: String,
}

/// Pasta das íntegras de uma sessão de agente, criada no primeiro corte.
#[derive(Debug)]
pub struct Spill {
    /// Raiz do agente; `None` = guarda desligada (só o corte).
    root: Option<PathBuf>,
    /// Nome base da pasta da sessão (id da sessão sem caracteres especiais).
    name: String,
    /// Pasta criada (absoluta) e o caminho relativo à raiz.
    dir: Option<(PathBuf, String)>,
    /// A criação falhou: não tenta de novo nesta sessão.
    broken: bool,
    seq: u32,
    used: u64,
    /// Apaga a pasta da sessão no fim (raiz que não é uma worktree).
    cleanup: bool,
}

impl Spill {
    /// Guarda as íntegras em `root`, numa pasta com o nome da sessão.
    pub fn new(root: &Path, session: &str) -> Self {
        // Numa worktree vinculada, `.git` é um arquivo: a pasta sai com a worktree.
        let worktree = fs::symlink_metadata(root.join(".git")).is_ok_and(|m| m.is_file());
        Self {
            root: Some(root.to_path_buf()),
            name: dir_name(session),
            dir: None,
            broken: false,
            seq: 0,
            used: 0,
            cleanup: !worktree,
        }
    }

    /// Sem guarda: o agente não lê arquivos (ou só lê a allowlist do relatório).
    pub fn disabled() -> Self {
        Self { root: None, name: String::new(), dir: None, broken: true, seq: 0, used: 0, cleanup: false }
    }

    /// Guarda `text` e devolve o caminho relativo à raiz; `None` acima dos tetos, com a guarda
    /// desligada ou se a escrita falhar (o corte segue sem arquivo).
    pub fn store(&mut self, text: &str) -> Option<String> {
        let len = u64::try_from(text.len()).ok()?;
        if self.broken || text.len() > MAX_FILE_BYTES || self.used.saturating_add(len) > MAX_SESSION_BYTES {
            return None;
        }
        let (abs, rel) = match &self.dir {
            Some(dir) => dir.clone(),
            None => match self.create_dir() {
                Ok(dir) => {
                    self.dir = Some(dir.clone());
                    dir
                }
                Err(e) => {
                    self.broken = true;
                    eprintln!("nants: saida de ferramenta sem arquivo (pasta): {}", e.kind());
                    return None;
                }
            },
        };
        self.seq += 1;
        let file = format!("{}.txt", self.seq);
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(abs.join(&file))
            .and_then(|mut f| f.write_all(text.as_bytes()));
        match written {
            Ok(()) => {
                self.used += len;
                Some(format!("{rel}/{file}"))
            }
            Err(e) => {
                eprintln!("nants: saida de ferramenta sem arquivo (escrita): {}", e.kind());
                let _ = fs::remove_file(abs.join(&file));
                None
            }
        }
    }

    /// Cria `.nants-outputs/<sessão>[-n]` (só do usuário), recusando symlinks no caminho e
    /// qualquer destino fora da raiz.
    fn create_dir(&self) -> io::Result<(PathBuf, String)> {
        let root = self.root.as_deref().ok_or_else(|| io::Error::other("sem raiz"))?;
        let canon_root = fs::canonicalize(root)?;
        let outputs = root.join(OUTPUTS_DIR);
        ensure_real_dir(&outputs)?;
        // A pasta se exclui do controle de versão sozinha, mesmo sem a linha no `info/exclude`.
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(outputs.join(".gitignore")) {
            Ok(mut f) => f.write_all(b"*\n")?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        for n in 1..=MAX_DIR_ATTEMPTS {
            let name = if n == 1 { self.name.clone() } else { format!("{}-{n}", self.name) };
            let abs = outputs.join(&name);
            match DirBuilder::new().mode(0o700).create(&abs) {
                Ok(()) => {
                    if !fs::canonicalize(&abs)?.starts_with(&canon_root) {
                        return Err(io::Error::other("fora da raiz"));
                    }
                    return Ok((abs, format!("{OUTPUTS_DIR}/{name}")));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("sem nome livre"))
    }
}

impl Drop for Spill {
    fn drop(&mut self) {
        if let (true, Some((abs, _))) = (self.cleanup, &self.dir) {
            let _ = fs::remove_dir_all(abs);
        }
    }
}

/// Pasta real (não symlink): existente, confere; ausente, cria só do usuário.
fn ensure_real_dir(dir: &Path) -> io::Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::other("não é pasta")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => match DirBuilder::new().mode(0o700).create(dir) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => ensure_real_dir(dir),
            other => other,
        },
        Err(e) => Err(e),
    }
}

/// Nome de pasta a partir do id da sessão (`developer:T02:a1` → `developer-T02-a1`).
fn dir_name(session: &str) -> String {
    let name: String = session
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' })
        .take(80)
        .collect();
    let name = name.trim_matches('-').to_string();
    if name.is_empty() {
        "sessao".to_string()
    } else {
        name
    }
}

/// Divide `budget` entre os tamanhos: quem cabe na parte igual fica inteiro e o restante é
/// redividido entre os maiores (a sobra de bytes vai aos primeiros).
pub fn allocate(lengths: &[usize], budget: usize) -> Vec<usize> {
    let mut out = vec![0; lengths.len()];
    let mut remaining = budget;
    let mut active: Vec<usize> = (0..lengths.len()).collect();
    while !active.is_empty() {
        let share = remaining / active.len();
        let (fixed, rest): (Vec<usize>, Vec<usize>) = active.iter().partition(|&&i| lengths[i] <= share);
        if fixed.is_empty() {
            let mut extra = remaining - share * active.len();
            for &i in &active {
                out[i] = share + usize::from(extra > 0);
                extra = extra.saturating_sub(1);
            }
            break;
        }
        for &i in &fixed {
            out[i] = lengths[i];
            remaining -= lengths[i];
        }
        active = rest;
    }
    out
}

/// Parte de cada resultado: o limite por resultado e, quando a resposta passa do orçamento
/// total, a divisão dele, com o piso [`MIN_SHARE_BYTES`].
fn shares(lengths: &[usize]) -> Vec<usize> {
    let capped: Vec<usize> = lengths.iter().map(|&l| l.min(MAX_TOOL_RESULT_BYTES)).collect();
    allocate(&capped, RESPONSE_BUDGET_BYTES)
        .into_iter()
        .zip(lengths)
        .map(|(share, &len)| share.max(MIN_SHARE_BYTES.min(len)))
        .collect()
}

/// Começo (1/5 de `share`) e fim (4/5) de `text`, com a linha do corte no meio. `path` é o
/// arquivo da íntegra, quando guardada.
pub fn cut(text: &str, share: usize, path: Option<&str>) -> String {
    let head_end = text.floor_char_boundary(share / 5);
    let tail_start = text.ceil_char_boundary(text.len().saturating_sub(share - share / 5)).max(head_end);
    let total = text.len();
    let omitted = tail_start - head_end;
    let line = match path {
        Some(path) => {
            let lines = text.split_inclusive('\n').count();
            format!(
                "{CUT_PREFIX}{omitted} de {total} bytes omitidos aqui; a íntegra ({total} bytes, {lines}{PATH_MARK}{path} \
                 e pode ser lida por faixas de linhas]"
            )
        }
        None => format!("{CUT_PREFIX}{omitted} de {total} bytes omitidos; a íntegra não foi guardada]"),
    };
    format!("{}\n{line}\n{}", &text[..head_end], &text[tail_start..])
}

/// Caminho da íntegra citado numa saída cortada, se houver.
pub fn stored_path(output: &str) -> Option<&str> {
    let line = output.lines().find(|l| l.starts_with(CUT_PREFIX))?;
    let (_, rest) = line.split_once(PATH_MARK)?;
    rest.split_whitespace().next()
}

/// Ajusta os resultados de uma resposta ao orçamento: os que passam da sua parte são cortados e
/// têm a íntegra guardada em `spill`. Devolve o que mudou (para o evento `spill`), ou `None`
/// sem corte.
pub fn fit(slots: &mut [Slot], spill: &mut Spill) -> Option<ContextChange> {
    let lengths: Vec<usize> = slots.iter().map(|s| s.content.len()).collect();
    let mut change = ContextChange::default();
    let mut tools: BTreeMap<String, u32> = BTreeMap::new();
    for (slot, share) in slots.iter_mut().zip(shares(&lengths)) {
        if slot.content.len() <= share {
            continue;
        }
        let path = spill.store(&slot.content);
        let shaped = cut(&slot.content, share, path.as_deref());
        change.before_bytes += u64::try_from(slot.content.len()).unwrap_or(u64::MAX);
        change.after_bytes += u64::try_from(shaped.len()).unwrap_or(u64::MAX);
        change.items += 1;
        *tools.entry(slot.tool.clone()).or_insert(0) += 1;
        slot.content = shaped;
    }
    change.tools = tools;
    (change.items > 0).then_some(change)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("nants-spill-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("mkdir");
        root
    }

    fn slot(tool: &str, content: String) -> Slot {
        Slot { tool: tool.into(), content }
    }

    #[test]
    fn allocate_keeps_small_whole_and_splits_the_rest() {
        assert_eq!(allocate(&[10, 20], 100), vec![10, 20]);
        assert_eq!(allocate(&[10, 500, 500], 100), vec![10, 45, 45]);
        assert_eq!(allocate(&[100, 100, 100], 100), vec![34, 33, 33]);
        assert_eq!(allocate(&[], 100), Vec::<usize>::new());
    }

    #[test]
    fn cut_keeps_head_and_tail_with_the_path_and_total() {
        let text = format!("exit: 1\n{}\nRESUMO FINAL", "ç linha\n".repeat(5000));
        let out = cut(&text, 10_000, Some(".nants-outputs/s/1.txt"));
        assert!(out.starts_with("exit: 1\n"), "{}", &out[..20]);
        assert!(out.ends_with("RESUMO FINAL"));
        assert!(out.len() <= 10_000 + 300, "{}", out.len());
        let line = out.lines().find(|l| l.starts_with(CUT_PREFIX)).expect("linha do corte");
        assert!(line.contains(&format!("{} bytes", text.len())) && line.contains("5003 linhas"), "{line}");
        assert_eq!(stored_path(&out), Some(".nants-outputs/s/1.txt"));
        let bare = cut(&text, 10_000, None);
        assert!(bare.contains("a íntegra não foi guardada") && stored_path(&bare).is_none());
        // Nada específico de ferramenta: o mesmo corte para qualquer texto.
        assert!(!out.contains("npm") && !out.contains("cargo"));
    }

    #[test]
    fn fit_stores_large_result_readable_and_private() {
        let root = temp_root("fit");
        let mut spill = Spill::new(&root, "developer:T02:a1");
        let big = format!("{}FIM\n", "abc\n".repeat(20_000));
        let mut slots = vec![slot("run_command", big.clone()), slot("list_dir", "a\nb".into())];
        let change = fit(&mut slots, &mut spill).expect("corte");
        assert_eq!((change.items, change.before_bytes), (1, big.len() as u64));
        assert_eq!(change.tools.get("run_command"), Some(&1));
        assert_eq!(slots[1].content, "a\nb", "o pequeno fica inteiro");
        assert!(slots[0].content.len() <= MAX_TOOL_RESULT_BYTES + 300);
        let rel = stored_path(&slots[0].content).expect("caminho");
        assert_eq!(rel, ".nants-outputs/developer-T02-a1/1.txt");
        assert_eq!(fs::read_to_string(root.join(rel)).expect("íntegra"), big);
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&root.join(rel)), 0o600);
        assert_eq!(mode(&root.join(".nants-outputs/developer-T02-a1")), 0o700);
        assert_eq!(fs::read_to_string(root.join(".nants-outputs/.gitignore")).expect("gi"), "*\n");
        // Fora de uma worktree, a pasta da sessão sai no fim da sessão.
        drop(spill);
        assert!(!root.join(".nants-outputs/developer-T02-a1").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn response_budget_splits_between_large_results() {
        let root = temp_root("budget");
        let mut spill = Spill::new(&root, "reviewer");
        let small = "s".repeat(2000);
        let mut slots: Vec<Slot> = (0..5).map(|i| slot("read_file", format!("{i}{}", "x".repeat(15_000)))).collect();
        slots.push(slot("search_code", small.clone()));
        let change = fit(&mut slots, &mut spill).expect("corte");
        assert_eq!(change.items, 5);
        assert_eq!(slots[5].content, small, "o pequeno fica inteiro");
        let total: usize = slots.iter().map(|s| s.content.len()).sum();
        assert!(total <= RESPONSE_BUDGET_BYTES + 5 * 300, "{total}");
        for (i, s) in slots.iter().take(5).enumerate() {
            let rel = stored_path(&s.content).expect("cada grande tem arquivo");
            assert!(fs::read_to_string(root.join(rel)).expect("íntegra").starts_with(&i.to_string()));
        }
        // Dentro do orçamento e do limite: nada muda e nada é gravado.
        let mut fine = vec![slot("read_file", "y".repeat(10_000)), slot("run_command", "z".repeat(10_000))];
        assert!(fit(&mut fine, &mut spill).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disk_caps_leave_only_the_cut() {
        let root = temp_root("caps");
        let mut spill = Spill::new(&root, "developer");
        assert!(spill.store(&"a".repeat(MAX_FILE_BYTES + 1)).is_none(), "teto por arquivo");
        assert!(!root.join(OUTPUTS_DIR).exists(), "nada criado acima do teto");
        spill.used = MAX_SESSION_BYTES - 10;
        assert!(spill.store(&"b".repeat(11)).is_none(), "teto da sessão");
        assert!(spill.store(&"c".repeat(10)).is_some());
        assert!(spill.store("d").is_none(), "sessão cheia");
        let mut slots = vec![slot("run_command", "e".repeat(40_000))];
        fit(&mut slots, &mut spill).expect("corte");
        assert!(slots[0].content.contains("a íntegra não foi guardada"));
        // Guarda desligada: só o corte.
        let mut off = Spill::disabled();
        let mut slots = vec![slot("run_command", "e".repeat(40_000))];
        fit(&mut slots, &mut off).expect("corte");
        assert!(stored_path(&slots[0].content).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinked_outputs_dir_is_refused() {
        let root = temp_root("symlink");
        let outside = temp_root("symlink-out");
        std::os::unix::fs::symlink(&outside, root.join(OUTPUTS_DIR)).expect("ln");
        let mut spill = Spill::new(&root, "developer");
        assert!(spill.store(&"a".repeat(100)).is_none());
        assert_eq!(fs::read_dir(&outside).expect("ls").count(), 0, "nada gravado fora da raiz");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=Nants Test", "-c", "user.email=test@example.com", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// As íntegras nunca entram no controle de versão (pelo `.gitignore` da pasta e pela linha do
    /// `info/exclude` do repositório, que vale para a worktree) e saem com a worktree.
    #[test]
    fn outputs_stay_out_of_version_control_and_leave_with_the_worktree() {
        let base = temp_root("git");
        let repo = base.join("repo");
        fs::create_dir_all(repo.join(".nants")).expect("mkdir");
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join(".nants/compose.yaml"), "services: {}\n").expect("w");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "inicial"]);
        let wt = base.join("wt");
        let wt_arg = wt.to_str().expect("utf8");
        git(&repo, &["worktree", "add", "-q", "-b", "sdd/x/T01", wt_arg]);
        let mut spill = Spill::new(&wt, "developer:T01:a1");
        let rel = spill.store("saída inteira\n").expect("guardada");
        drop(spill);
        assert!(wt.join(&rel).exists(), "numa worktree a íntegra fica até a worktree sair");
        // `.nants/` (contexto do build e base do hash do ambiente) fica intocada.
        assert_eq!(fs::read_dir(wt.join(".nants")).expect("ls").count(), 1);
        let clean = |dir: &Path| {
            git(dir, &["add", "-A"]);
            assert_eq!(git(dir, &["status", "--porcelain", "--untracked-files=all"]), "", "nada a versionar");
        };
        clean(&wt);
        // Só a linha do `info/exclude` do repositório também basta para a worktree.
        fs::remove_file(wt.join(OUTPUTS_DIR).join(".gitignore")).expect("rm");
        fs::write(repo.join(".git/info/exclude"), ".nants-outputs/\n").expect("exclude");
        clean(&wt);
        git(&repo, &["worktree", "remove", "--force", wt_arg]);
        assert!(!wt.exists(), "a íntegra sai com a worktree");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sessions_get_their_own_dir() {
        let root = temp_root("dirs");
        fs::write(root.join(".git"), "gitdir: /x\n").expect("worktree");
        let mut a = Spill::new(&root, "developer:T01:a1");
        let mut b = Spill::new(&root, "developer:T01:a1");
        assert_eq!(a.store("1").as_deref(), Some(".nants-outputs/developer-T01-a1/1.txt"));
        assert_eq!(b.store("2").as_deref(), Some(".nants-outputs/developer-T01-a1-2/1.txt"));
        drop(a);
        // Numa worktree, a pasta fica até a worktree sair.
        assert!(root.join(".nants-outputs/developer-T01-a1/1.txt").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
