//! Spec 133 T06 (A1): conferência mecânica dos caminhos que o Revisor cita como evidência.
//!
//! A verificação final do Test 10 aprovou 8 de 8 critérios citando caminhos que não existiam. Aqui
//! o parecer estruturado é conferido contra a árvore revisada, sem linguagem nem framework:
//! - **fonte:** só `criteria[].evidence` (critérios `met`) e `findings[].location`, nunca a prosa;
//! - **o que não é caminho:** URL (`://`), glob (`*`, `?`, `{`), tipo MIME (`image/png`), `a/b` sem
//!   extensão nem linha e rota absoluta que não existe sob a raiz (`/api/users.json`); pela forma
//!   (spec 137 T06), endereço IP, digest e pacote (`@scope/pkg@1.2.3`, `pkg==1.2.3`,
//!   `group:artifact:version`) e, quando o primeiro segmento não existe na árvore, a imagem com
//!   host (`mcr.microsoft.com/openjdk/jdk:21`) ou sem host (`bitnami/postgresql:16`);
//! - **existência:** os arquivos e diretórios de `git ls-tree -r -t <rev>` (o HEAD da branch na
//!   revisão da tarefa, a base na verificação final) e o disco da worktree, inclusive o que o git
//!   ignora (capturas, `test-results`, arquivos gerados), comparados pelo sufixo (a citação é o fim do caminho real);
//! - **evidência inválida:** um caminho com linha que não existe, a linha que passa do fim do
//!   arquivo, ou nenhum dos caminhos citados existe. No apontamento (`location`), só as duas
//!   primeiras regras: citar um arquivo que falta, sem linha, é um apontamento legítimo.
//!
//! Sem conferência de trechos. O laço do Revisor ([`crate::agents::llm_agents`]) devolve a lista
//! ao Revisor uma vez, na mesma sessão; o que continuar inválido fica "não verificado".

use std::collections::{BTreeMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::agents::usage::ContextChange;
use crate::harness::git::Git;
use crate::i18n::{self, Lang};

use super::runner::{ReviewResult, WarningStatus};

/// Teto de entradas da varredura do disco (arquivos e diretórios, em largura): a varredura só
/// roda quando a árvore do git e o caminho exato no disco não acharam o citado.
const DISK_WALK_MAX: usize = 20_000;
/// Arquivo maior que isto não tem as linhas contadas (a linha citada é aceita).
const LINE_COUNT_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Tipos de topo dos tipos MIME (`image/png`, `application/vnd.api+json`).
const MIME_TOP: [&str; 9] = ["application", "audio", "font", "image", "message", "model", "multipart", "text", "video"];

/// Um caminho citado: o texto como o Revisor escreveu (`src/a.rs:10`), o caminho e a linha inicial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub cited: String,
    pub path: String,
    pub line: Option<u32>,
    /// Trecho entre crases ou aspas com espaço (`` `node --test src/a.js` ``): as partes divididas por
    /// espaço que parecem caminho. O trecho inteiro só vale se existir (caminho com espaço); senão,
    /// valem as partes, e o trecho não conta como citação.
    pub parts: Vec<Citation>,
}

/// Separadores entre citações fora de crases e aspas.
fn is_separator(c: char) -> bool {
    c.is_whitespace() || c == ',' || c == ';'
}

/// Tokens do campo: o que está entre crases ou aspas duplas vai inteiro (caminho com espaço), com o
/// que vier colado depois do fechamento (`` `src/a.js`:12 ``); o resto é dividido por espaço, `,` e `;`.
fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' || c == '"' {
            let mut quoted = String::new();
            let mut closed = false;
            for q in chars.by_ref() {
                if q == c {
                    closed = true;
                    break;
                }
                quoted.push(q);
            }
            current.push_str(&quoted);
            if !closed {
                // Sem fechamento: o resto segue como texto fora das aspas.
                out.extend(current.split(is_separator).filter(|p| !p.is_empty()).map(str::to_string));
                current.clear();
                continue;
            }
            while let Some(&next) = chars.peek() {
                if is_separator(next) || next == '`' || next == '"' {
                    break;
                }
                current.push(next);
                chars.next();
            }
            out.push(std::mem::take(&mut current));
        } else if is_separator(c) {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

const EDGES_START: [char; 7] = ['`', '"', '\'', '(', '[', '<', '*'];
const EDGES_END: [char; 10] = ['`', '"', '\'', ')', ']', '>', '.', ',', ':', '*'];

/// Normaliza um token em `(caminho, linha)`: tira a pontuação e o negrito das bordas, fica com o
/// destino do link markdown (`[a](src/a.js)`), corta em `::` (id de teste), em `#` (âncora) e no
/// primeiro `:` (`src/app.py:main`), lê a linha de `:N` ou `:a-b` e troca a barra invertida por `/`.
/// `None` quando não parece caminho: sem `/`, só conta com linha.
fn normalize(token: &str) -> Option<(String, Option<u32>)> {
    let t = token.trim();
    let t = t.rsplit_once("](").map_or(t, |(_, target)| target);
    let t = t.trim_start_matches(EDGES_START).trim_end_matches(EDGES_END);
    if t.contains("://") || is_package_ref(t) {
        return None;
    }
    let t = t.split("::").next().unwrap_or_default();
    let t = t.split('#').next().unwrap_or_default();
    let (path, line) = match t.find(':') {
        Some(i) => {
            let digits: String = t[i + 1..].chars().take_while(char::is_ascii_digit).collect();
            // `host:5000/...`: a porta seguida de `/` é endereço, não arquivo com linha (spec 137 T06).
            if !digits.is_empty() && t[i + 1 + digits.len()..].starts_with('/') {
                return None;
            }
            (&t[..i], digits.parse::<u32>().ok())
        }
        None => (t, None),
    };
    let path = path.trim_start_matches(EDGES_START).trim_end_matches(EDGES_END).replace('\\', "/");
    if path.is_empty() || !(path.contains('/') || (path.contains('.') && line.is_some())) {
        return None;
    }
    Some((path, line))
}

/// Caminhos citados num campo do parecer, sem repetição, já sem URL, glob, MIME e `a/b` sem
/// extensão nem linha.
pub fn citations(text: &str) -> Vec<Citation> {
    let mut out: Vec<Citation> = Vec::new();
    let push = |c: Citation, out: &mut Vec<Citation>| {
        if !out.iter().any(|o| o.cited == c.cited) {
            out.push(c);
        }
    };
    for token in tokens(text) {
        if !token.contains(is_separator) {
            if let Some(c) = citation(&token) {
                push(c, &mut out);
            }
            continue;
        }
        // Trecho entre crases ou aspas com espaço: comando, frase ou caminho com espaço.
        let mut parts: Vec<Citation> = Vec::new();
        for part in token.split(is_separator).filter_map(citation) {
            push(part, &mut parts);
        }
        match citation(&token) {
            Some(whole) => push(Citation { parts, ..whole }, &mut out),
            None => parts.into_iter().for_each(|p| push(p, &mut out)),
        }
    }
    out
}

/// Uma citação de um token, sem partes; `None` para o que não é caminho (URL, glob, MIME, `a/b`,
/// endereço IP, digest e pacote).
fn citation(token: &str) -> Option<Citation> {
    let (path, line) = normalize(token)?;
    if path.contains(['*', '?', '{', '}']) || is_mime(&path, line.is_some()) || is_ipv4(first_segment(&path)) {
        return None;
    }
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    if line.is_none() && !last.contains('.') {
        return None;
    }
    let cited = match line {
        Some(l) => format!("{path}:{l}"),
        None => path.clone(),
    };
    Some(Citation { cited, path, line, parts: Vec::new() })
}

/// `image/png`, `text/html; charset=utf-8`, `application/vnd.api+json`: um `/`, tipo de topo conhecido, sem linha.
fn is_mime(path: &str, has_line: bool) -> bool {
    if has_line {
        return false;
    }
    match path.split_once('/') {
        Some((top, sub)) => MIME_TOP.contains(&top.to_ascii_lowercase().as_str()) && !sub.is_empty() && !sub.contains('/'),
        None => false,
    }
}

/// Spec 137 T06: referência de pacote ou de imagem pela forma, sem tabela por linguagem nem por
/// registro: digest (`nome@sha256:…`), versão com `@` (`@scope/pkg@1.2.3`, `pkg@^1.2`), requisito
/// (`pkg==1.2.3`, `pkg>=2`) e coordenada `group:artifact:version` (três partes, sem `/`, a segunda
/// sem ser número, para não confundir com `a.js:10:5`).
fn is_package_ref(t: &str) -> bool {
    if ["==", "~=", ">=", "<="].iter().any(|op| t.contains(op)) {
        return true;
    }
    if let Some((algo, hash)) = t.rsplit_once('@').and_then(|(_, after)| after.split_once(':')) {
        let digest = !algo.is_empty()
            && algo.chars().all(|c| c.is_ascii_alphanumeric())
            && hash.len() >= 7
            && hash.chars().all(|c| c.is_ascii_hexdigit());
        if digest {
            return true;
        }
    }
    let versioned = t.split('/').any(|seg| match seg.find('@') {
        Some(i) if i > 0 => is_version(&seg[i + 1..]),
        _ => false,
    });
    if versioned {
        return true;
    }
    let parts: Vec<&str> = t.split(':').collect();
    !t.contains('/') && parts.len() >= 3 && parts.iter().all(|p| !p.is_empty()) && !parts[1].chars().all(|c| c.is_ascii_digit())
}

/// `1.2.3`, `^1.2`, `~4`, `2.0.0-beta.1`: número de versão, não nome de arquivo (`logo@2x.png`).
fn is_version(v: &str) -> bool {
    let v = v.trim_start_matches(['^', '~']);
    let (core, extra) = match v.find(['-', '+']) {
        Some(i) => (&v[..i], Some(&v[i + 1..])),
        None => (v, None),
    };
    !core.is_empty()
        && core.split('.').all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        && extra.is_none_or(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
}

/// O primeiro segmento de um caminho (`a` em `a/b/c`), sem a barra inicial.
fn first_segment(path: &str) -> &str {
    path.trim_start_matches('/').split('/').next().unwrap_or_default()
}

/// Endereço IPv4 literal (`127.0.0.1`).
fn is_ipv4(seg: &str) -> bool {
    let octets: Vec<&str> = seg.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|o| (1..=3).contains(&o.len()) && o.chars().all(|c| c.is_ascii_digit()) && o.parse::<u16>().is_ok_and(|n| n <= 255))
}

/// Segmento com cara de host: rótulos de domínio separados por ponto, o último só com letras
/// (`mcr.microsoft.com`, `ghcr.io`). `.nants` e `docs.v2` não têm essa forma.
fn is_host_like(seg: &str) -> bool {
    let labels: Vec<&str> = seg.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Resultado de procurar uma citação na árvore.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Located {
    /// Rota absoluta que não existe sob a raiz: não é caminho do projeto, não conta.
    NotAPath,
    Missing,
    /// Existe; com a maior contagem de linhas entre os arquivos que casam (`None`: não deu para contar).
    Found(Option<usize>),
}

/// A árvore revisada: os arquivos e diretórios de `rev` e o disco da worktree.
pub struct Tree {
    root: PathBuf,
    rev: String,
    entries: Vec<String>,
    disk: OnceLock<Vec<String>>,
}

impl Tree {
    /// `None` quando o git não lê a árvore de `rev` (sem repositório, sem a branch): sem conferência.
    pub fn load(root: &Path, rev: &str) -> Option<Self> {
        let entries = Git::new(root).ls_tree_all(rev).ok()?;
        Some(Self { root: root.to_path_buf(), rev: rev.to_string(), entries, disk: OnceLock::new() })
    }

    /// O citado relativo à raiz e se é uma rota absoluta: tira o prefixo da própria raiz, o `/work/`
    /// do contêiner, `./` e `/`. Só é absoluto o que começa com `/` sem nenhum desses prefixos.
    fn relative(&self, path: &str) -> (String, bool) {
        let p = path.trim().replace('\\', "/");
        let root = self.root.to_string_lossy().replace('\\', "/");
        let root = root.trim_end_matches('/');
        let stripped = match p.strip_prefix(root) {
            Some(rest) if !root.is_empty() && rest.starts_with('/') => Some(rest),
            _ => p.strip_prefix("/work/"),
        };
        let absolute = stripped.is_none() && p.starts_with('/');
        let p = stripped.unwrap_or(&p);
        (p.trim_start_matches("./").trim_start_matches('/').trim_end_matches('/').to_string(), absolute)
    }

    /// Arquivos e diretórios do disco (relativos, com `/`), em largura, sem `.git` e sem seguir links.
    fn disk_entries(&self) -> &[String] {
        self.disk.get_or_init(|| {
            let mut out = Vec::new();
            let mut queue: VecDeque<(PathBuf, String)> = VecDeque::from([(self.root.clone(), String::new())]);
            while let Some((dir, prefix)) = queue.pop_front() {
                let Ok(read) = std::fs::read_dir(&dir) else { continue };
                for entry in read.flatten() {
                    if out.len() >= DISK_WALK_MAX {
                        return out;
                    }
                    let Ok(name) = entry.file_name().into_string() else { continue };
                    if name == ".git" {
                        continue;
                    }
                    let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                    if entry.file_type().is_ok_and(|t| t.is_dir()) {
                        queue.push_back((entry.path(), rel.clone()));
                    }
                    out.push(rel);
                }
            }
            out
        })
    }

    /// O caminho relativo fica sob a raiz (sem `..`).
    fn inside(rel: &str) -> bool {
        !rel.is_empty() && Path::new(rel).components().all(|c| matches!(c, Component::Normal(_)))
    }

    /// Linhas de um arquivo da árvore do git (`in_git`) ou do disco; `None` sem conteúdo legível.
    fn line_count(&self, rel: &str, in_git: bool) -> Option<usize> {
        let bytes = if in_git {
            Git::new(&self.root).show_bytes(&self.rev, rel)?
        } else {
            let path = self.root.join(rel);
            let meta = std::fs::metadata(&path).ok()?;
            if !meta.is_file() || meta.len() > LINE_COUNT_MAX_BYTES {
                return None;
            }
            std::fs::read(&path).ok()?
        };
        let newlines = bytes.iter().filter(|b| **b == b'\n').count();
        Some(newlines + usize::from(bytes.last().is_some_and(|b| *b != b'\n')))
    }

    /// Spec 137 T06: o citado que não existe tem a forma de uma referência de imagem, e não de
    /// arquivo, quando o primeiro segmento é seguido de `/` e não existe na árvore, e:
    /// - **host:** o segmento tem cara de host (`mcr.microsoft.com/openjdk/jdk:21`, `ghcr.io/o/a`);
    /// - **imagem sem host:** `a/b:N` com o último segmento sem extensão (`bitnami/postgresql:16`).
    ///
    /// Um citado de um segmento só (`README.md:1`, `web.config:3`) nunca tem essa forma.
    fn reference_shape(&self, rel: &str, has_line: bool) -> bool {
        let Some((first, rest)) = rel.split_once('/') else { return false };
        if first.is_empty() || rest.is_empty() {
            return false;
        }
        let last = rest.rsplit('/').next().unwrap_or_default();
        (is_host_like(first) || (has_line && !last.contains('.'))) && !self.segment_exists(first)
    }

    /// Algum arquivo ou diretório da árvore (git ou disco) tem `seg` como um dos componentes.
    fn segment_exists(&self, seg: &str) -> bool {
        let has = |e: &String| e.split('/').any(|p| p == seg);
        self.entries.iter().any(has) || self.root.join(seg).exists() || self.disk_entries().iter().any(has)
    }

    fn locate(&self, c: &Citation) -> Located {
        let (rel, absolute) = self.relative(&c.path);
        if !Self::inside(&rel) {
            return if absolute { Located::NotAPath } else { Located::Missing };
        }
        // Rota absoluta: só o caminho exato sob a raiz. O relativo casa também quando é o fim do
        // caminho real (`app.js` acha `src/app.js`), num só sentido: `src/live/index.js` não é `index.js`.
        let suffix = format!("/{rel}");
        let matches = |e: &String| e == &rel || (!absolute && e.ends_with(&suffix));
        let mut found: Vec<(String, bool)> = self.entries.iter().filter(|e| matches(e)).map(|e| (e.clone(), true)).collect();
        let on_disk = self.root.join(&rel);
        if found.is_empty() && on_disk.exists() {
            found.push((rel.clone(), false));
        }
        if found.is_empty() {
            found = self.disk_entries().iter().filter(|e| matches(e)).map(|e| (e.clone(), false)).collect();
        }
        if found.is_empty() {
            return if absolute || self.reference_shape(&rel, c.line.is_some()) { Located::NotAPath } else { Located::Missing };
        }
        if c.line.is_none() {
            return Located::Found(None);
        }
        Located::Found(found.iter().filter_map(|(path, in_git)| self.line_count(path, *in_git)).max())
    }
}

/// Um problema de uma citação.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// O caminho citado não existe na árvore revisada.
    Missing(String),
    /// A linha citada passa do fim do arquivo (`lines` linhas).
    PastEnd { cited: String, lines: usize },
}

impl Problem {
    /// Texto da citação para o Revisor e para a matriz (`caminho` ou `caminho:linha`).
    pub fn cited(&self) -> &str {
        match self {
            Self::Missing(c) | Self::PastEnd { cited: c, .. } => c,
        }
    }
}

/// Item do parecer com evidência inválida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// Índice em `criteria`.
    Criterion(usize),
    /// Índice em `findings`.
    Finding(usize),
    /// Índice em `warnings`: o aviso do Desenvolvedor dado como `confirmed` (spec 136 T09).
    Warning(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub item: Item,
    /// O texto do critério, o id do apontamento ou o número do aviso.
    pub label: String,
    pub problems: Vec<Problem>,
}

/// Resultado da conferência de um parecer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Audit {
    /// Citações conferidas (sem as rotas absolutas que não existem).
    pub cited: u32,
    pub missing: u32,
    pub past_end: u32,
    pub issues: Vec<Issue>,
}

impl Audit {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// Problemas do critério `index`, se a evidência dele ficou inválida.
    pub fn criterion(&self, index: usize) -> Option<&Issue> {
        self.issues.iter().find(|i| i.item == Item::Criterion(index))
    }
}

/// Confere as citações de um campo. `strict`: um caminho sem linha que falta só invalida quando
/// nenhum citado existe (critério); sem `strict` (apontamento), só caminho com linha conta.
fn check_field(tree: &Tree, text: &str, strict: bool, audit: &mut Audit) -> Vec<Problem> {
    let (mut counted, mut existing) = (0u32, 0u32);
    let mut problems = Vec::new();
    let mut unlined = Vec::new();
    // Trecho com espaço: vale inteiro se existir (caminho com espaço); senão, valem as partes e o
    // trecho não conta (comando ou frase que cita um arquivo).
    let mut located: Vec<(Citation, Located)> = Vec::new();
    for c in citations(text) {
        let spaced = c.path.contains(is_separator);
        let found = tree.locate(&c);
        let chosen = if spaced && !matches!(found, Located::Found(_)) {
            c.parts.into_iter().map(|p| {
                let l = tree.locate(&p);
                (p, l)
            }).collect()
        } else {
            vec![(c, found)]
        };
        for (c, l) in chosen {
            if !located.iter().any(|(o, _)| o.cited == c.cited) {
                located.push((c, l));
            }
        }
    }
    for (c, found) in located {
        match found {
            Located::NotAPath => continue,
            Located::Missing => {
                counted += 1;
                if c.line.is_some() {
                    problems.push(Problem::Missing(c.cited));
                } else {
                    unlined.push(Problem::Missing(c.cited));
                }
            }
            Located::Found(lines) => {
                counted += 1;
                existing += 1;
                if let (Some(line), Some(n)) = (c.line, lines) {
                    if usize::try_from(line).unwrap_or(usize::MAX) > n {
                        problems.push(Problem::PastEnd { cited: c.cited, lines: n });
                    }
                }
            }
        }
    }
    audit.cited += counted;
    if strict && counted > 0 && (existing == 0 || !problems.is_empty()) {
        problems.extend(unlined);
    }
    audit.missing += problems.iter().filter(|p| matches!(p, Problem::Missing(_))).count() as u32;
    audit.past_end += problems.iter().filter(|p| matches!(p, Problem::PastEnd { .. })).count() as u32;
    problems
}

/// Confere o parecer: a evidência dos critérios `met`, o `location` dos apontamentos e a evidência
/// dos avisos do Desenvolvedor dados como `confirmed` (spec 136 T09, com a régua dos critérios).
pub fn audit(result: &ReviewResult, tree: &Tree) -> Audit {
    let mut audit = Audit::default();
    for (i, c) in result.criteria.iter().enumerate() {
        if !c.met {
            continue;
        }
        let problems = check_field(tree, &c.evidence, true, &mut audit);
        if !problems.is_empty() {
            audit.issues.push(Issue { item: Item::Criterion(i), label: c.criterion.clone(), problems });
        }
    }
    for (i, f) in result.findings.iter().enumerate() {
        let problems = check_field(tree, &f.location, false, &mut audit);
        if !problems.is_empty() {
            audit.issues.push(Issue { item: Item::Finding(i), label: f.id.clone(), problems });
        }
    }
    audit_warnings_into(result, tree, &mut audit);
    audit
}

/// Só a parte dos avisos `confirmed` de [`audit`]: a conferência dos desfechos que chegam no novo
/// pedido dos avisos (spec 136 T09), sem recontar critérios e apontamentos já conferidos.
pub fn audit_warnings(result: &ReviewResult, tree: &Tree) -> Audit {
    let mut audit = Audit::default();
    audit_warnings_into(result, tree, &mut audit);
    audit
}

fn audit_warnings_into(result: &ReviewResult, tree: &Tree, audit: &mut Audit) {
    for (i, w) in result.warnings.iter().enumerate() {
        if w.status != WarningStatus::Confirmed {
            continue;
        }
        let problems = check_field(tree, &w.evidence, true, audit);
        if !problems.is_empty() {
            audit.issues.push(Issue { item: Item::Warning(i), label: w.number.to_string(), problems });
        }
    }
}

/// Lista curta das citações com problema (`a.rs:10, b.rs:400`), para a matriz.
pub fn cited_list(issue: &Issue) -> String {
    issue.problems.iter().map(Problem::cited).collect::<Vec<_>>().join(", ")
}

/// Marca como "não verificado" os critérios do parecer com a evidência ainda inválida: `met` vira
/// `false` com `unverified`, e a evidência ganha o motivo no idioma do projeto. Só a resposta do
/// Revisor faz um critério "não cumprido". O aviso `confirmed` com a evidência inválida passa a
/// `not_checkable`, com o mesmo motivo, e vai para as pendências como os outros não conferidos
/// (spec 136 T09).
pub fn mark_unverified(lang: Lang, result: &mut ReviewResult, audit: &Audit) {
    for issue in &audit.issues {
        let Item::Warning(i) = issue.item else { continue };
        if let Some(w) = result.warnings.get_mut(i) {
            w.status = WarningStatus::NotCheckable;
            w.evidence = i18n::tr(
                lang,
                "runner.review.unverified_evidence",
                &[("paths", &cited_list(issue)), ("evidence", w.evidence.trim())],
            );
        }
    }
    for (i, c) in result.criteria.iter_mut().enumerate() {
        if let Some(issue) = audit.criterion(i) {
            c.met = false;
            c.unverified = true;
            c.evidence = i18n::tr(
                lang,
                "runner.review.unverified_evidence",
                &[("paths", &cited_list(issue)), ("evidence", c.evidence.trim())],
            );
        }
    }
}

/// O evento da conferência para o `context.jsonl` (spec 133 T06): só contagens e o desfecho
/// (`ok`, `reask`, `fixed` ou `unverified`).
pub fn event(audit: &Audit, reason: &'static str) -> ContextChange {
    ContextChange {
        items: u32::try_from(audit.issues.len()).unwrap_or(u32::MAX),
        tools: BTreeMap::from([
            ("cited".to_string(), audit.cited),
            ("missing".to_string(), audit.missing),
            ("past_end".to_string(), audit.past_end),
        ]),
        reason: Some(reason),
        ..ContextChange::default()
    }
}

/// Spec 133 T13 (D10): radicais (em minúsculas, nos dois idiomas do produto) das palavras com que
/// uma pendência declara algo que só a execução prova: um teste, um comportamento não provado, algo
/// que não rodou. Marcador lido por parser, não texto de pessoa. Casam com o começo de cada palavra.
const UNPROVEN_STEMS: [&str; 12] =
    ["test", "untested", "unproven", "provad", "provar", "provou", "comprov", "behavio", "comportament", "execut", "rodou", "rodad"];
/// Palavras inteiras do mesmo marcador, que como radical pegariam outras (`prove` em `provedor`).
const UNPROVEN_WORDS: [&str; 5] = ["prove", "proved", "proven", "proving", "rodar"];

/// Spec 133 T13 (D10): a pendência fala de comportamento não provado (um teste, algo que precisa
/// rodar): só evidência de execução a resolve.
pub fn claims_unproven(item: &str) -> bool {
    item.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| UNPROVEN_WORDS.contains(&word) || UNPROVEN_STEMS.iter().any(|stem| word.starts_with(stem)))
}

/// Spec 133 T13 (D10): o caminho tem a forma de um arquivo de teste, em qualquer linguagem: um
/// diretório `test*`, `spec`, `specs`, `__tests__` ou `e2e`, ou o nome `test_x`, `x_test`, `x-test`,
/// `XTest`, `XTests`, `x.test.ext`, `x.spec.ext`, `x_spec`.
pub fn looks_like_test_path(path: &str) -> bool {
    let path = path.replace('\\', "/");
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let Some(name) = segments.pop() else {
        return false;
    };
    let dir = segments.iter().any(|d| {
        let d = d.to_lowercase();
        d.starts_with("test") || matches!(d.as_str(), "spec" | "specs" | "__tests__" | "e2e")
    });
    let mut parts = name.split('.');
    let stem = parts.next().unwrap_or_default();
    let lower = stem.to_lowercase();
    let dotted = parts.any(|p| matches!(p.to_lowercase().as_str(), "test" | "tests" | "spec"));
    let named = lower.starts_with("test")
        || ["_test", "-test", "_tests", "-tests", "_spec", "-spec"].iter().any(|s| lower.ends_with(s))
        || stem.ends_with("Test")
        || stem.ends_with("Tests");
    dir || dotted || named
}

/// Spec 133 T13 (D10): a evidência cita um arquivo de teste que existe na árvore revisada (pelo
/// caminho, com ou sem linha, ou pelo nome do arquivo). Sem árvore, vale a forma do caminho.
fn cites_test_file(tree: Option<&Tree>, evidence: &str) -> bool {
    let mut candidates = citations(evidence);
    for token in tokens(evidence).iter().flat_map(|t| t.split(is_separator).map(str::to_string).collect::<Vec<_>>()) {
        let t = token.trim_start_matches(EDGES_START).trim_end_matches(EDGES_END);
        let t = t.split("::").next().unwrap_or_default();
        let t = t.split(':').next().unwrap_or_default();
        if t.contains('.') && !t.contains('/') && !t.contains("://") && looks_like_test_path(t) {
            candidates.push(Citation { cited: t.to_string(), path: t.to_string(), line: None, parts: Vec::new() });
        }
    }
    candidates
        .iter()
        .flat_map(|c| std::iter::once(c).chain(c.parts.iter()))
        .filter(|c| looks_like_test_path(&c.path))
        .any(|c| tree.is_none_or(|tree| matches!(tree.locate(c), Located::Found(_))))
}

/// Spec 133 T13 (D10): a evidência cita algo que aparece numa saída de comando registrada na sessão
/// (a saída dos testes e das checagens que o Revisor recebeu): um identificador com cara de nome
/// de teste ou de caminho (com `_`, `:`, `.`, `/` ou `-` por dentro, ou em camelCase), com letra, e
/// presente na saída.
fn cites_command_output(evidence: &str, outputs: &[&str]) -> bool {
    tokens(evidence)
        .iter()
        .flat_map(|t| t.split(is_separator).map(str::to_string).collect::<Vec<_>>())
        .any(|token| {
            let t = token.trim_start_matches(EDGES_START).trim_end_matches(EDGES_END);
            let chars: Vec<char> = t.chars().collect();
            let inner = chars.len() > 2
                && (chars[1..chars.len() - 1].iter().any(|c| matches!(c, '_' | ':' | '.' | '/' | '-'))
                    || chars.windows(2).any(|w| w[0].is_lowercase() && w[1].is_uppercase()));
            inner && chars.iter().any(|c| c.is_alphabetic()) && outputs.iter().any(|o| o.contains(t))
        })
}

/// Spec 133 T13 (D10): por que uma pendência dada como resolvida na verificação final não vale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingCheck {
    Valid,
    /// A evidência cita caminho que não existe (ou linha além do fim), pela regra dos critérios.
    Paths,
    /// A pendência fala de comportamento não provado e a evidência não é de execução.
    NoExecution,
}

/// Spec 133 T13 (D10): confere a evidência de uma pendência resolvida. Primeiro a mesma conferência
/// de caminhos dos critérios (sem árvore, pulada); depois, se o texto da pendência fala de
/// comportamento não provado ([`claims_unproven`]), a evidência precisa ser de execução: um arquivo
/// de teste que existe ([`looks_like_test_path`]) ou um nome que aparece numa saída de comando
/// registrada na sessão (`outputs`). Arquivo de configuração não basta.
pub fn check_pending(tree: Option<&Tree>, item: &str, evidence: &str, outputs: &[&str]) -> PendingCheck {
    if let Some(tree) = tree {
        let mut audit = Audit::default();
        if !check_field(tree, evidence, true, &mut audit).is_empty() {
            return PendingCheck::Paths;
        }
    }
    if !claims_unproven(item) || cites_test_file(tree, evidence) || cites_command_output(evidence, outputs) {
        PendingCheck::Valid
    } else {
        PendingCheck::NoExecution
    }
}

/// Spec 133 T13 (D10): as pendências listadas nos FATOS da verificação final
/// (`- Txx P-yy: texto`, ver `final_observations_facts`), como `(tarefa, id, texto)`.
pub fn pending_items(observations: &str) -> Vec<(String, String, String)> {
    observations
        .lines()
        .filter_map(|line| {
            let (head, text) = line.trim().strip_prefix("- ")?.split_once(':')?;
            let mut words = head.split_whitespace();
            let (task, id) = (words.next()?, words.next()?);
            (words.next().is_none() && id.starts_with("P-")).then(|| (task.to_string(), id.to_string(), text.trim().to_string()))
        })
        .collect()
}

/// Spec 133 T13 (D10): reabre as pendências que a verificação final deu como resolvidas sem
/// evidência válida ([`check_pending`]): `resolved` volta a `false`, e a pendência fica aberta e
/// vai ao relatório. Devolve quantas foram reabertas.
pub fn reopen_weak_pending(result: &mut ReviewResult, observations: &str, tree: Option<&Tree>, outputs: &[&str]) -> usize {
    let items = pending_items(observations);
    let mut reopened = 0;
    for reply in result.pending.iter_mut().filter(|r| r.resolved) {
        let item = items
            .iter()
            .find(|(task, id, _)| task.eq_ignore_ascii_case(reply.task.trim()) && id.eq_ignore_ascii_case(reply.id.trim()))
            .map_or("", |(_, _, text)| text.as_str());
        if check_pending(tree, item, &reply.evidence, outputs) != PendingCheck::Valid {
            reply.resolved = false;
            reopened += 1;
        }
    }
    reopened
}

/// Pedido ao Revisor (prompt de modelo, não texto de pessoa): a lista do que não existe na árvore
/// revisada e o pedido de refazer a evidência com caminhos reais. Vai como turno do usuário no meio
/// da sessão do Revisor, então sai no idioma do projeto (spec 133 T11).
pub fn reask_prompt(audit: &Audit, criteria: &[String], lang: crate::i18n::Lang) -> String {
    use crate::i18n::{tr, tr_plural};
    let mut lines = Vec::new();
    for issue in &audit.issues {
        let what = match issue.item {
            Item::Criterion(i) => {
                tr(lang, "agent.evidence.criterion", &[("label", criteria.get(i).map_or(issue.label.as_str(), String::as_str))])
            }
            Item::Finding(_) => tr(lang, "agent.evidence.finding", &[("label", &issue.label)]),
            Item::Warning(_) => tr(lang, "agent.evidence.warning", &[("label", &issue.label)]),
        };
        let problems = issue
            .problems
            .iter()
            .map(|p| match p {
                Problem::Missing(c) => tr(lang, "agent.evidence.missing", &[("path", c)]),
                Problem::PastEnd { cited, lines } => tr_plural(lang, "agent.evidence.past_end", *lines as u64, &[("path", cited)]),
            })
            .collect::<Vec<_>>()
            .join("; ");
        lines.push(format!("- {what}: {problems}"));
    }
    tr(lang, "agent.evidence.reask", &[("list", &lines.join("\n"))])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::runner::CriterionCheck;
    use crate::sdd::{Finding, Verdict};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(root)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Repositório com `src/app.js` (3 linhas) em `main` e `.gitignore` de `test-results/`.
    fn repo(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!("nants-evidence-{}-{}-{tag}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("dir");
        std::fs::write(root.join("src/app.js"), "a\nb\nc\n").expect("w");
        std::fs::write(root.join(".gitignore"), "test-results/\n").expect("w");
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "i"]);
        root
    }

    fn result(evidence: &[&str], locations: &[&str]) -> ReviewResult {
        ReviewResult {
            verdict: Verdict::Approved,
            summary: "s".into(),
            findings: locations
                .iter()
                .enumerate()
                .map(|(i, l)| Finding { id: format!("F-{:02}", i + 1), location: (*l).into(), ..Finding::default() })
                .collect(),
            criteria: evidence
                .iter()
                .enumerate()
                .map(|(i, e)| CriterionCheck { criterion: format!("c{i}"), met: true, evidence: (*e).into(), unverified: false })
                .collect(),
            screenshots_viewed: Vec::new(),
            declared: Vec::new(),
            pending: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Spec 133 T13 (D10): o que conta como pendência de comportamento e como arquivo de teste.
    #[test]
    fn unproven_claims_and_test_paths_are_recognized() {
        for item in ["modo offline não provado", "Developer warning: offline not proven", "não testado no navegador", "the behavior is untested", "o comando não rodou"] {
            assert!(claims_unproven(item), "{item}");
        }
        for item in ["a lista não pagina", "README sem a seção de uso", "o provedor de dados está em src/x.rs"] {
            assert!(!claims_unproven(item), "{item}");
        }
        for path in ["tests/a.rs", "src/app.test.ts", "e2e/home.spec.ts", "pkg/app_test.go", "test_app.py", "src/AppTest.java", "spec/user_spec.rb", "src/__tests__/a.js"] {
            assert!(looks_like_test_path(path), "{path}");
        }
        for path in ["go.mod", "src/app.js", "package.json", "src/latest.json", "src/contest.rs"] {
            assert!(!looks_like_test_path(path), "{path}");
        }
    }

    /// Spec 133 T13 (D10): a pendência de comportamento com evidência de configuração fica aberta; com
    /// um teste que existe, ou com o nome que aparece na saída registrada, é resolvida; caminho que
    /// não existe invalida qualquer pendência.
    #[test]
    fn pending_evidence_is_checked_by_paths_and_execution() {
        let root = repo("pending");
        std::fs::write(root.join("go.mod"), "module x\n").expect("w");
        std::fs::create_dir_all(root.join("tests")).expect("dir");
        std::fs::write(root.join("tests/offline_test.go"), "a\nb\n").expect("w");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "p"]);
        let tree = Tree::load(&root, "main").expect("árvore");
        let unproven = "Aviso do Desenvolvedor: offline não provado";
        assert_eq!(check_pending(Some(&tree), unproven, "go.mod:1", &[]), PendingCheck::NoExecution);
        assert_eq!(check_pending(Some(&tree), unproven, "tests/offline_test.go:2", &[]), PendingCheck::Valid);
        assert_eq!(check_pending(Some(&tree), unproven, "coberto por offline_test.go", &[]), PendingCheck::Valid);
        assert_eq!(check_pending(Some(&tree), unproven, "tests/sumiu_test.go:2", &[]), PendingCheck::Paths);
        assert_eq!(check_pending(Some(&tree), unproven, "pkg/sumiu_test.go", &[]), PendingCheck::Paths, "teste que não existe");
        let output = "--- PASS: TestOfflineVotes (0.01s)\nok  example/pkg 0.2s";
        assert_eq!(check_pending(Some(&tree), unproven, "TestOfflineVotes passou", &[output]), PendingCheck::Valid);
        assert_eq!(check_pending(Some(&tree), unproven, "TestOtherThing passou", &[output]), PendingCheck::NoExecution);
        assert_eq!(check_pending(Some(&tree), unproven, "ok, passou", &[output]), PendingCheck::NoExecution, "palavra comum não conta");
        // A pendência que não fala de comportamento segue a conferência de caminhos.
        assert_eq!(check_pending(Some(&tree), "a lista não pagina", "src/app.js:2", &[]), PendingCheck::Valid);
        assert_eq!(check_pending(Some(&tree), "a lista não pagina", "src/app.js:40", &[]), PendingCheck::Paths);
        // Sem árvore: sem conferência de caminhos; o teste vale pela forma.
        assert_eq!(check_pending(None, unproven, "tests/offline_test.go:2", &[]), PendingCheck::Valid);
        assert_eq!(check_pending(None, unproven, "go.mod:1", &[]), PendingCheck::NoExecution);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pending_items_are_read_from_the_final_facts() {
        let facts = "- T01 P-01: offline não provado\n- T01: F-05 [MEDIUM] a.js:1: obs\n- T02 P-03: x: y\n";
        assert_eq!(
            pending_items(facts),
            [("T01".into(), "P-01".into(), "offline não provado".into()), ("T02".into(), "P-03".into(), "x: y".into())]
        );
    }

    #[test]
    fn url_route_glob_and_mime_are_not_checked() {
        let text = "https://example.com/a/b.js:10 http://localhost:3000/api GET /api/users/1.json tests/**/*.test.js \
                    src/{a,b}.js image/png application/vnd.api+json text/html src/components docs/a?.md";
        let cited: Vec<String> = citations(text).into_iter().map(|c| c.cited).collect();
        assert_eq!(cited, ["/api/users/1.json"]);
        let root = repo("excluded");
        let tree = Tree::load(&root, "main").expect("árvore");
        let audit = audit(&result(&[text], &["/api/users/1.json:3"]), &tree);
        assert!(audit.is_clean() && audit.cited == 0, "{audit:?}");
        // Caminho de verdade continua conferido (com linha, sem linha e no estilo do contêiner).
        let cited: Vec<String> = citations("`src/app.js:2`, README.md:1 /work/src/app.js (lib/x.py)").into_iter().map(|c| c.cited).collect();
        assert_eq!(cited, ["src/app.js:2", "README.md:1", "/work/src/app.js", "lib/x.py"]);
    }

    /// F-01 e F-04 da revisão: id de teste, símbolo, âncora, crase antes da linha, espaço entre
    /// crases e barra invertida citam o arquivo.
    #[test]
    fn citation_suffixes_quotes_and_backslashes_are_normalized() {
        let text = "tests/test_x.py::test_y src/app.py:main src/app.js#L2 `src/a.js`:12 `docs/Guia de uso.md:1` src\\app.js:2 [src/b.js:3]";
        let cited: Vec<(String, Option<u32>)> = citations(text).into_iter().map(|c| (c.path, c.line)).collect();
        assert_eq!(
            cited,
            [
                ("tests/test_x.py".to_string(), None),
                ("src/app.py".to_string(), None),
                ("src/app.js".to_string(), None),
                ("src/a.js".to_string(), Some(12)),
                ("docs/Guia de uso.md".to_string(), Some(1)),
                ("src/app.js".to_string(), Some(2)),
                ("src/b.js".to_string(), Some(3)),
            ]
        );
        let root = repo("normalize");
        std::fs::create_dir_all(root.join("tests")).expect("dir");
        std::fs::create_dir_all(root.join("docs")).expect("dir");
        std::fs::write(root.join("tests/test_x.py"), "x\n").expect("w");
        std::fs::write(root.join("src/app.py"), "x\n").expect("w");
        std::fs::write(root.join("docs/Guia de uso.md"), "x\n").expect("w");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "n"]);
        let tree = Tree::load(&root, "main").expect("árvore");
        let evidence = ["tests/test_x.py::test_y", "src/app.py:main", "src/app.js#L2", "`docs/Guia de uso.md:1`", "src\\app.js:2"];
        let audit = audit(&result(&evidence, &[]), &tree);
        assert!(audit.is_clean() && audit.cited == 5, "{audit:?}");
    }

    /// F-05 da revisão: comando ou frase entre crases ou aspas que cita um arquivo existente nunca
    /// invalida; o trecho inteiro só vale como caminho com espaço quando existe. Também os formatos
    /// comuns de prosa (lista, vírgula, ponto final, parênteses, negrito, link markdown).
    #[test]
    fn quoted_commands_and_sentences_check_their_paths_not_the_whole_text() {
        let root = repo("quoted");
        std::fs::create_dir_all(root.join("tests")).expect("dir");
        std::fs::create_dir_all(root.join("docs")).expect("dir");
        std::fs::write(root.join("tests/test_x.py"), "x\n").expect("w");
        std::fs::write(root.join("docs/Guia de uso.md"), "x\n").expect("w");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "q"]);
        let tree = Tree::load(&root, "main").expect("árvore");
        for (evidence, cited) in [
            ("`node --test src/app.js` passou", 1),
            ("`npx vitest run src/app.js:2`", 1),
            ("\"falha em src/app.js:3\" foi corrigida", 1),
            ("`docs/Guia de uso.md:1`", 1),
            ("`pytest tests/test_x.py::test_y -q`", 1),
            ("Implementado em src/app.js:2, testado em tests/test_x.py.", 2),
            ("- src/app.js:1\n- tests/test_x.py", 2),
            ("(ver src/app.js:3). **src/app.js** e [app](src/app.js:1)", 1 + 1 + 1),
            ("`src/app.js`:2 e 'src/app.js:1'", 2),
        ] {
            let audit = audit(&result(&[evidence], &[]), &tree);
            assert!(audit.is_clean(), "{evidence}: {audit:?}");
            assert_eq!(audit.cited, cited, "{evidence}: {audit:?}");
        }
        // Comando que cita um arquivo inexistente, como única citação, continua invalidando.
        let audit = audit(&result(&["`node --test src/nope.js`"], &[]), &tree);
        assert_eq!(audit.criterion(0).map(|i| i.problems.clone()), Some(vec![Problem::Missing("src/nope.js".into())]));
        // Na linha que passa do fim, dentro de um comando, também.
        let past = super::audit(&result(&["`npx vitest run src/app.js:9`"], &[]), &tree);
        assert_eq!(past.criterion(0).map(|i| i.problems.clone()), Some(vec![Problem::PastEnd { cited: "src/app.js:9".into(), lines: 3 }]));
    }

    /// F-02 e F-03 da revisão: a citação casa só como fim do caminho real, e o prefixo do projeto
    /// (`/work/` ou a raiz) não transforma o caminho em rota.
    #[test]
    fn suffix_matches_one_way_and_project_prefixes_are_checked() {
        let root = repo("one-way");
        std::fs::write(root.join("index.js"), "x\n").expect("w");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "idx"]);
        let tree = Tree::load(&root, "main").expect("árvore");
        let c = |t: &str| citations(t).remove(0);
        assert_eq!(tree.locate(&c("src/live/index.js:1")), Located::Missing);
        assert_eq!(tree.locate(&c("index.js:1")), Located::Found(Some(1)));
        assert_eq!(tree.locate(&c("app.js:1")), Located::Found(Some(3)));
        assert_eq!(tree.locate(&c("/work/src/nope.js:1")), Located::Missing);
        let inside = format!("{}/src/nope.js:3", root.display());
        assert_eq!(tree.locate(&c(&inside)), Located::Missing);
        assert_eq!(tree.locate(&c("/api/nope.json:1")), Located::NotAPath);
        let audit = audit(&result(&["/work/src/nope.js:1"], &[]), &tree);
        assert_eq!(audit.criterion(0).map(|i| i.problems.clone()), Some(vec![Problem::Missing("/work/src/nope.js:1".into())]));
    }

    /// Spec 137 T06: imagem, host, IP e pacote não são caminho, pela forma do token; o que é arquivo
    /// continua conferido.
    #[test]
    fn image_host_ip_and_package_references_are_not_paths() {
        // (b) IP e (d) digest e pacote: fora já na leitura das citações.
        for text in [
            "127.0.0.1:8080/api",
            "127.0.0.1/api",
            "10.0.0.1:5432",
            "registry.example.com:5000/team/app:1",
            "node@sha256:4f1c2a9be0d1e2f3a4b5c6d7e8f9",
            "mcr.microsoft.com/openjdk/jdk@sha256:abcdef0123456789",
            "org.springframework.boot:spring-boot-starter-web:3.2.0",
            "com.example:demo:1.0",
            "@scope/pkg@1.2.3",
            "@angular/core@^17.0.1",
            "pkg==1.2.3",
            "django>=4.2",
        ] {
            assert!(citations(text).is_empty(), "{text}: {:?}", citations(text));
        }
        // Linha e coluna, e nome de arquivo com `@`, continuam citação.
        let cited: Vec<String> = citations("src/app.js:10:5 app.js:3:1 img/logo@2x.png").into_iter().map(|c| c.cited).collect();
        assert_eq!(cited, ["src/app.js:10", "app.js:3", "img/logo@2x.png"]);

        let root = repo("images");
        std::fs::create_dir_all(root.join(".nants")).expect("dir");
        std::fs::create_dir_all(root.join("docs.v2")).expect("dir");
        std::fs::write(root.join(".nants/Dockerfile"), "FROM mcr.microsoft.com/openjdk/jdk:21\nWORKDIR /app\n").expect("w");
        std::fs::write(root.join("docs.v2/x.md"), "x\n").expect("w");
        std::fs::write(root.join("web.config"), "a\nb\nc\n").expect("w");
        std::fs::write(root.join("README.md"), "r\n").expect("w");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "img"]);
        let tree = Tree::load(&root, "main").expect("árvore");
        let c = |t: &str| citations(t).remove(0);
        // (a) host e (c) imagem sem host, com o primeiro segmento fora da árvore.
        for t in [
            "mcr.microsoft.com/openjdk/jdk:21",
            "ghcr.io/org/app:1.4",
            "docker.io/library/node:20",
            "github.com/org/repo.git",
            "bitnami/postgresql:16",
            "library/node:20",
            "bin/run:3",
        ] {
            assert_eq!(tree.locate(&c(t)), Located::NotAPath, "{t}");
        }
        // Nada afrouxa: arquivo real com ponto no primeiro segmento e o de um segmento só.
        assert_eq!(tree.locate(&c(".nants/Dockerfile:1")), Located::Found(Some(2)));
        assert_eq!(tree.locate(&c("docs.v2/x.md:1")), Located::Found(Some(1)));
        assert_eq!(tree.locate(&c("web.config:3")), Located::Found(Some(3)));
        assert_eq!(tree.locate(&c("README.md:99")), Located::Found(Some(1)));
        for t in ["README.md:1", "app.js:3", "web.config:3"] {
            assert!(!tree.reference_shape(&c(t).path, true), "{t}");
        }
        // Primeiro segmento que existe: continua conferido e acusado se falta.
        for t in ["docs.v2/nope.md:1", "src/run:3", ".nants/sumiu:1", "src/nope.js:2"] {
            assert_eq!(tree.locate(&c(t)), Located::Missing, "{t}");
        }
        let checks = [
            ("README.md:99", vec![Problem::PastEnd { cited: "README.md:99".into(), lines: 1 }]),
            ("nao-existe.md:1", vec![Problem::Missing("nao-existe.md:1".into())]),
            ("web.config:9", vec![Problem::PastEnd { cited: "web.config:9".into(), lines: 3 }]),
            ("docs.v2/x.md:5", vec![Problem::PastEnd { cited: "docs.v2/x.md:5".into(), lines: 1 }]),
            ("/work/.nants/Dockerfile:9", vec![Problem::PastEnd { cited: "/work/.nants/Dockerfile:9".into(), lines: 2 }]),
        ];
        for (evidence, problems) in checks {
            let audit = audit(&result(&[evidence], &[]), &tree);
            assert_eq!(audit.criterion(0).map(|i| i.problems.clone()), Some(problems), "{evidence}");
        }
        // README.md:99 quando o README não existe continua acusado.
        std::fs::remove_file(root.join("README.md")).expect("rm");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "sem readme"]);
        let tree = Tree::load(&root, "main").expect("árvore");
        let gone = audit(&result(&["README.md:99"], &[]), &tree);
        assert_eq!(gone.criterion(0).map(|i| i.problems.clone()), Some(vec![Problem::Missing("README.md:99".into())]));

        // O caso do test-13 inteiro: a evidência do G3 passa a valer.
        let g3 = "A imagem base está em `.nants/Dockerfile:1`; conferido com \
                  `grep \"mcr.microsoft.com/openjdk/jdk:21\" .nants/Dockerfile`.";
        let mut r = result(&[g3], &[]);
        let audit = audit(&r, &tree);
        assert!(audit.is_clean(), "{audit:?}");
        assert_eq!(audit.cited, 1, "{audit:?}");
        mark_unverified(Lang::En, &mut r, &audit);
        assert!(r.criteria[0].met && !r.criteria[0].unverified);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legitimate_evidence_is_clean_and_suffix_matches() {
        let root = repo("legit");
        let tree = Tree::load(&root, "main").expect("árvore");
        let audit = audit(&result(&["app.js:3 e src/app.js", "/work/src/app.js:1", "visto no test_run"], &["src/app.js:2"]), &tree);
        assert!(audit.is_clean(), "{audit:?}");
        assert_eq!(audit.cited, 4);
    }

    #[test]
    fn missing_path_and_line_past_end_invalidate() {
        let root = repo("missing");
        let tree = Tree::load(&root, "main").expect("árvore");
        let r = result(
            &["tests/unit/auth.test.mjs:12 e src/app.js:1", "src/app.js:40", "src/live/projections.mjs", "src/app.js:2 src/extra.js"],
            &["src/nope.js:4", "tests/falta.test.js"],
        );
        let audit = audit(&r, &tree);
        assert_eq!(audit.criterion(0).map(|i| i.problems.clone()), Some(vec![Problem::Missing("tests/unit/auth.test.mjs:12".into())]));
        assert_eq!(audit.criterion(1).map(|i| i.problems.clone()), Some(vec![Problem::PastEnd { cited: "src/app.js:40".into(), lines: 3 }]));
        assert_eq!(audit.criterion(2).map(|i| i.problems.clone()), Some(vec![Problem::Missing("src/live/projections.mjs".into())]));
        // Um caminho sem linha que falta, com outro que existe: a evidência vale.
        assert!(audit.criterion(3).is_none(), "{audit:?}");
        // No apontamento, só o caminho com linha conta: o arquivo que falta é um apontamento legítimo.
        assert!(audit.issues.iter().any(|i| i.item == Item::Finding(0)));
        assert!(!audit.issues.iter().any(|i| i.item == Item::Finding(1)));
        assert_eq!((audit.missing, audit.past_end), (3, 1));
        // Critério not_met não é conferido.
        let mut unmet = r.clone();
        unmet.criteria.iter_mut().for_each(|c| c.met = false);
        unmet.findings.clear();
        assert!(super::audit(&unmet, &tree).is_clean());
    }

    #[test]
    fn ignored_and_generated_files_on_disk_count_as_existing() {
        let root = repo("ignored");
        std::fs::create_dir_all(root.join("test-results/run-1")).expect("dir");
        std::fs::write(root.join("test-results/run-1/home.png"), [0u8, 1, 2]).expect("w");
        std::fs::write(root.join("test-results/report.txt"), "1\n2\n").expect("w");
        std::fs::write(root.join("src/gerado.js"), "x\n").expect("w");
        let tree = Tree::load(&root, "main").expect("árvore");
        let audit = audit(&result(&["run-1/home.png", "test-results/report.txt:2", "src/gerado.js:1"], &[]), &tree);
        assert!(audit.is_clean(), "{audit:?}");
        let past = super::audit(&result(&["test-results/report.txt:9"], &[]), &tree);
        assert_eq!(past.criterion(0).map(|i| i.problems.len()), Some(1));
    }

    #[test]
    fn final_verification_checks_against_the_base() {
        let root = repo("base");
        git(&root, &["checkout", "-q", "-b", "feat"]);
        std::fs::write(root.join("src/so-na-branch.js"), "1\n").expect("w");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "b"]);
        git(&root, &["checkout", "-q", "main"]);
        let r = result(&["src/so-na-branch.js:1"], &[]);
        assert!(audit(&r, &Tree::load(&root, "feat").expect("branch")).is_clean());
        let base = audit(&r, &Tree::load(&root, "main").expect("base"));
        assert_eq!(base.criterion(0).map(|i| i.problems.clone()), Some(vec![Problem::Missing("src/so-na-branch.js:1".into())]));
        assert!(Tree::load(&root, "nao-existe").is_none());
    }

    #[test]
    fn reask_prompt_lists_each_problem() {
        let audit = Audit {
            issues: vec![
                Issue { item: Item::Criterion(0), label: "c0".into(), problems: vec![Problem::Missing("a.rs:1".into())] },
                Issue { item: Item::Finding(0), label: "F-01".into(), problems: vec![Problem::PastEnd { cited: "b.rs:9".into(), lines: 3 }] },
            ],
            ..Audit::default()
        };
        let text = reask_prompt(&audit, &["login funciona".into()], crate::i18n::Lang::PtBr);
        assert!(text.contains("- Critério \"login funciona\": `a.rs:1` não existe"), "{text}");
        assert!(text.contains("- Apontamento F-01: `b.rs:9` passa do fim do arquivo (3 linhas)"), "{text}");
        // Spec 133 T11: num projeto em inglês, a repergunta inteira sai em inglês.
        let en = reask_prompt(&audit, &["login works".into()], crate::i18n::Lang::En);
        assert!(en.contains("- Criterion \"login works\": `a.rs:1` does not exist"), "{en}");
        assert!(en.contains("- Finding F-01: `b.rs:9` goes past the end of the file (3 lines)"), "{en}");
        assert!(en.starts_with("The Nants check did not find"), "{en}");
    }

    /// Spec 136 T09: a evidência de um aviso `confirmed` passa pela mesma conferência de caminhos
    /// dos critérios. A que cita o que não existe vai ao pedido único e, se continuar inválida,
    /// vira `not_checkable` com o motivo, e o aviso cai nas pendências (não fica "conferido").
    /// Os outros desfechos não são conferidos aqui.
    #[test]
    fn confirmed_warning_evidence_is_checked_by_paths() {
        use crate::agents::runner::{apply_warnings, WarningReply};
        let root = repo("warnings");
        let tree = Tree::load(&root, "main").expect("árvore");
        let reply = |number: usize, status: WarningStatus, evidence: &str| WarningReply {
            number,
            status,
            evidence: evidence.into(),
            finding: String::new(),
        };
        let mut r = result(&[], &[]);
        r.warnings = vec![
            reply(1, WarningStatus::Confirmed, "src/app.js:2"),
            reply(2, WarningStatus::Confirmed, "src/ApiExceptionHandler.java:41"),
            reply(3, WarningStatus::NotCheckable, "src/sumiu.rs:1 não está no projeto"),
            reply(4, WarningStatus::Confirmed, "src/app.js:40"),
        ];
        let audit = audit(&r, &tree);
        let items: Vec<(Item, &str)> = audit.issues.iter().map(|i| (i.item.clone(), i.label.as_str())).collect();
        assert_eq!(items, vec![(Item::Warning(1), "2"), (Item::Warning(3), "4")]);
        let en = reask_prompt(&audit, &[], crate::i18n::Lang::En);
        assert!(en.contains("- Developer warning 2 (confirmed): `src/ApiExceptionHandler.java:41` does not exist"), "{en}");
        let pt = reask_prompt(&audit, &[], crate::i18n::Lang::PtBr);
        assert!(pt.contains("- Aviso 4 do Desenvolvedor (confirmed): `src/app.js:40` passa do fim do arquivo (3 linhas)"), "{pt}");
        mark_unverified(crate::i18n::Lang::En, &mut r, &audit);
        assert_eq!(r.warnings[0].status, WarningStatus::Confirmed);
        assert_eq!(r.warnings[1].status, WarningStatus::NotCheckable);
        assert!(r.warnings[1].evidence.starts_with("not verified:"), "{}", r.warnings[1].evidence);
        assert_eq!((r.warnings[2].status, r.warnings[3].status), (WarningStatus::NotCheckable, WarningStatus::NotCheckable));
        let texts: Vec<String> = (1..=4).map(|n| format!("aviso {n}")).collect();
        let (out, outcome) = apply_warnings(crate::i18n::Lang::En, r, &texts);
        assert_eq!(out.verdict, Verdict::Approved);
        assert_eq!(outcome.checked.len(), 1, "{outcome:?}");
        assert!(outcome.checked[0].contains("aviso 1"), "{outcome:?}");
        assert_eq!(outcome.pending.len(), 3, "{outcome:?}");
        assert!(outcome.pending.iter().any(|p| p.contains("aviso 2") && p.contains("ApiExceptionHandler.java:41")), "{outcome:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
