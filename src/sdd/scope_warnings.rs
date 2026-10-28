//! Avisos do plano: critério/objetivo de tarefa que cita um caminho fora do `allowed_scope`.
//! Função pura; os avisos nunca bloqueiam (heurística: falsos positivos são tolerados).
//!
//! Spec 094 T02 (sem falso positivo): não acusam a menção a extensão (`.svg`, `*.ts`), os
//! diretórios e artefatos citados numa frase de exclusão do versionamento (`.gitignore` ignora
//! `node_modules/`) e o nome solto cujo caminho completo, dentro do escopo, a própria tarefa cita.

use super::Task;
use crate::harness::tools::glob_match_path;

// Títulos estruturais (`sdd::section_titles`): os mesmos que o runner lê.
const OBJECTIVE: &str = crate::agents::runner::OBJECTIVE;
const ACCEPTANCE: &str = crate::agents::runner::ACCEPTANCE;

/// Com mais avisos de caminho que isto na mesma tarefa, [`grouped_scope_warnings`] os junta numa
/// linha só.
const GROUP_ABOVE: usize = 3;

/// Avisos de uma tarefa: os do `allowed_scope` (`.nants/**`) e os caminhos citados fora do escopo
/// (rótulo da seção, caminho), na ordem do plano.
struct TaskWarnings {
    scope: Vec<String>,
    cited: Vec<(&'static str, String)>,
}

/// Um aviso por (tarefa, caminho citado fora do escopo), na ordem do plano. Formato:
/// `Txx: critério cita `deps.edn`, fora do escopo` (ou `objetivo cita`, conforme a seção).
pub fn scope_warnings(tasks: &[Task]) -> Vec<String> {
    let mut out = Vec::new();
    for task in tasks {
        let id = &task.frontmatter.id;
        let w = task_warnings(task);
        out.extend(w.scope);
        out.extend(w.cited.iter().map(|(label, path)| cited_line(id, label, path)));
    }
    out
}

/// Como [`scope_warnings`], mas mais de 3 caminhos citados na mesma tarefa viram uma linha só:
/// `Txx: N caminhos citados fora do escopo (`a`, `b`, `c` e mais N−3)`. É a forma mostrada na UI
/// e devolvida ao Arquiteto na autocorreção do plano (spec 092 T01).
pub fn grouped_scope_warnings(tasks: &[Task]) -> Vec<String> {
    let mut out = Vec::new();
    for task in tasks {
        let id = &task.frontmatter.id;
        let w = task_warnings(task);
        out.extend(w.scope);
        if w.cited.len() > GROUP_ABOVE {
            let first: Vec<String> = w.cited.iter().take(GROUP_ABOVE).map(|(_, p)| format!("`{p}`")).collect();
            out.push(format!(
                "{id}: {} caminhos citados fora do escopo ({} e mais {})",
                w.cited.len(),
                first.join(", "),
                w.cited.len() - GROUP_ABOVE
            ));
        } else {
            out.extend(w.cited.iter().map(|(label, path)| cited_line(id, label, path)));
        }
    }
    out
}

fn cited_line(id: &str, label: &str, path: &str) -> String {
    format!("{id}: {label} cita `{path}`, fora do escopo")
}

fn task_warnings(task: &Task) -> TaskWarnings {
    let meta = &task.frontmatter;
    let mut w = TaskWarnings { scope: Vec::new(), cited: Vec::new() };
    if meta.allowed_scope.is_empty() {
        return w;
    }
    // `.nants/**` nunca entra no escopo da tarefa (só `request_environment_change` altera o ambiente).
    for glob in &meta.allowed_scope {
        let g = glob.trim().trim_start_matches("./");
        if is_env_path(g) {
            w.scope.push(format!(
                "{}: allowed_scope inclui `{g}`, que não entra no escopo (arquivos de ambiente só via request_environment_change)",
                meta.id
            ));
        }
    }
    let sections: Vec<(&'static str, String)> = [(OBJECTIVE, "objetivo"), (ACCEPTANCE, "critério")]
        .into_iter()
        .filter_map(|(title, label)| crate::agents::runner::section_text(&task.body, title).map(|t| (label, t)))
        .collect();
    // Caminhos com diretório (sem glob) citados pela tarefa e dentro do escopo: cobrem o nome
    // solto com o mesmo nome de arquivo.
    let cited_in_scope: Vec<String> = sections
        .iter()
        .flat_map(|(_, text)| path_tokens(text))
        .filter(|p| p.contains('/') && !p.contains('*') && in_scope(&meta.allowed_scope, p))
        .collect();
    let mut seen: Vec<String> = Vec::new();
    for (label, text) in &sections {
        let segments: Vec<(bool, Vec<String>)> =
            segments(text).iter().map(|s| (is_exclusion_context(s), path_tokens(s))).collect();
        for path in path_tokens(text) {
            if seen.contains(&path)
                || is_extension_mention(&path)
                || suppressed_by_exclusion(&path, &segments)
                || in_task_scope(&meta.allowed_scope, &path, &cited_in_scope)
            {
                continue;
            }
            w.cited.push((label, path.clone()));
            seen.push(path);
        }
    }
    w
}

/// Escopo da tarefa para um caminho citado. Nome de arquivo solto (sem `/`) só conta com entrada
/// literal no `allowed_scope` ou com o caminho completo citado pela tarefa dentro do escopo: um
/// `**` sozinho nunca o cobre (`deps.edn` com `src/**` acusa). O resto segue o glob.
fn in_task_scope(globs: &[String], path: &str, cited_in_scope: &[String]) -> bool {
    if !is_bare_file_name(path) {
        return in_scope(globs, path);
    }
    let literal = globs.iter().any(|g| {
        let g = g.trim().trim_start_matches("./");
        let literal = !g.contains('*') && (g == path || g.rsplit('/').next() == Some(path));
        // Glob na raiz (`*.md`, sem `/` e sem `**`) cobre o nome solto que casa com ele.
        let root_glob = !g.contains('/') && g.contains('*') && !g.contains("**") && glob_match_path(g, path);
        literal || root_glob
    });
    literal || cited_in_scope.iter().any(|c| c.rsplit('/').next() == Some(path))
}

/// Nome de arquivo sem diretório: sem `/`, sem glob, com extensão ou nome conhecido.
fn is_bare_file_name(path: &str) -> bool {
    !path.contains('/') && !path.contains('*') && (has_file_extension(path) || KNOWN_NAMES.contains(&path))
}

/// Dotfiles conhecidos cujo nome também é uma extensão de [`FILE_EXTENSIONS`]: são arquivos, não
/// menção a extensão (`.gitignore` acusa; `.svg` não).
const DOTFILE_NAMES: &[&str] = &[
    ".gitignore", ".gitattributes", ".dockerignore", ".npmignore", ".env", ".npmrc", ".nvmrc", ".yarnrc",
    ".editorconfig", ".prettierrc", ".eslintrc", ".babelrc", ".stylelintrc", ".browserslistrc", ".htaccess",
];

/// Menção a extensão, sem diretório: `.<ext>` (que não seja dotfile conhecido) ou `*.<ext>`.
fn is_extension_mention(path: &str) -> bool {
    if path.contains('/') || !has_file_extension(path) {
        return false;
    }
    let rest = path.strip_prefix("*.").or_else(|| path.strip_prefix('.'));
    match rest {
        Some(ext) if path.starts_with('*') => !ext.contains('*'),
        Some(ext) => !ext.contains('.') && !DOTFILE_NAMES.contains(&path.to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// Termos de exclusão do versionamento (minúsculas). "excluir" sozinho (apagar) não conta.
const EXCLUSION_TERMS: &[&str] = &[
    "não versionar",
    "não versionad",
    "nao versionar",
    "nao versionad",
    "ignorad",
    "ignora",
    "fora do versionamento",
    "excluído do versionamento",
    "excluída do versionamento",
    "excluídos do versionamento",
    "excluídas do versionamento",
    "excluido do versionamento",
    "excluida do versionamento",
    "excluidos do versionamento",
    "excluidas do versionamento",
    "excluir do git",
    "exclude",
    "não commitar",
    "nao commitar",
    "fora do repositório",
    "fora do repositorio",
];

/// A frase tem termo de exclusão do versionamento fora das crases (o termo dentro de um caminho
/// não conta).
fn is_exclusion_context(segment: &str) -> bool {
    let plain: String = segment.split('`').step_by(2).collect::<Vec<_>>().join(" ").to_lowercase();
    EXCLUSION_TERMS.iter().any(|t| plain.contains(t))
}

/// Frases do texto: cada linha (item de lista) e, dentro dela, cada frase terminada por `.`, `!` ou
/// `?` seguido de espaço, fora das crases.
fn segments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut current = String::new();
        let mut in_code = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            current.push(c);
            if c == '`' {
                in_code = !in_code;
            } else if !in_code && ".!?".contains(c) && chars.peek().is_some_and(|n| n.is_whitespace()) {
                out.push(std::mem::take(&mut current));
            }
        }
        if !current.trim().is_empty() {
            out.push(current);
        }
    }
    out
}

/// Diretório ou artefato (terminado em `/`, glob de diretório ou sem extensão de arquivo) citado
/// só em frases de exclusão do versionamento. Arquivo com extensão, nome conhecido e o próprio
/// arquivo de ignore nunca são suprimidos.
fn suppressed_by_exclusion(path: &str, segments: &[(bool, Vec<String>)]) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ignore_file = name.starts_with('.') && name.to_ascii_lowercase().ends_with("ignore");
    if has_file_extension(path) || KNOWN_NAMES.contains(&name) || ignore_file {
        return false;
    }
    let mut cited = segments.iter().filter(|(_, tokens)| tokens.contains(&path.to_string())).peekable();
    cited.peek().is_some() && cited.all(|(exclusion, _)| *exclusion)
}

fn in_scope(globs: &[String], path: &str) -> bool {
    let dir_probe = format!("{path}/x");
    globs.iter().any(|g| {
        let g = g.trim().trim_start_matches("./");
        glob_match_path(g, path) || glob_match_path(g, &dir_probe)
    })
}

/// Tokens com cara de caminho: entre crases, ou palavras soltas com `/` e extensão conhecida ou glob.
pub fn path_tokens(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |token: Option<String>| {
        if let Some(p) = token {
            if !found.contains(&p) {
                found.push(p);
            }
        }
    };
    let mut plain = String::new();
    for (i, part) in text.split('`').enumerate() {
        if i % 2 == 1 {
            push(known_name(part).or_else(|| normalize(part)));
        } else {
            plain.push_str(part);
            plain.push(' ');
        }
    }
    for word in plain.split_whitespace() {
        let w = word.trim_matches(|c: char| ",;:()[]\"'".contains(c)).trim_end_matches('.');
        let w = strip_emphasis(w).trim_matches(|c: char| ",;:()[]\"'".contains(c)).trim_end_matches('.');
        if w.contains('/') && (has_file_extension(w) || is_glob(w)) {
            push(normalize(w));
        }
    }
    found
}

/// Extensões de arquivo reconhecidas (código, marcação, estilo, dados, imagem, configuração,
/// scripts). Fora da lista, `a/b.c` não é caminho (ex.: `pageerror/console.error`).
const FILE_EXTENSIONS: &[&str] = &[
    // código
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rb", "go", "java", "kt", "kts", "scala", "clj", "cljs",
    "cljc", "edn", "ex", "exs", "erl", "hs", "ml", "c", "h", "cc", "cpp", "hpp", "cs", "fs", "swift", "php", "lua",
    "dart", "vue", "svelte", "astro", "elm", "zig", "sol", "wasm",
    // marcação e templates
    "html", "htm", "xml", "md", "mdx", "rst", "txt", "adoc", "svg", "ejs", "hbs", "pug", "njk", "liquid", "j2",
    "tmpl", "ipynb",
    // estilo
    "css", "scss", "sass", "less", "styl",
    // dados
    "json", "jsonl", "yaml", "yml", "toml", "csv", "tsv", "sql", "graphql", "gql", "proto", "db", "sqlite", "log",
    "pdf", "zip", "map",
    // imagem e fontes
    "png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "bmp", "woff", "woff2", "ttf", "otf",
    // configuração
    "ini", "cfg", "conf", "env", "lock", "properties", "gradle", "tf", "hcl", "gitignore", "gitattributes",
    "dockerignore", "editorconfig", "npmrc", "nvmrc", "prettierrc", "eslintrc", "babelrc",
    // scripts
    "sh", "bash", "zsh", "ps1", "bat", "cmd", "mk",
    // mídia e 3D
    "mp3", "wav", "ogg", "m4a", "flac", "mp4", "webm", "mov", "glb", "gltf",
    // manifestos e afins
    "mod", "sum", "mts", "cts", "webmanifest", "prisma", "lockb",
];

/// Tira a ênfase de markdown das pontas (`**a/b**`, `*a/b*`, `_a/b_`): o mesmo número de `*`/`_`
/// dos dois lados. Glob solto (`e2e/**`, `**/*.ts`) não tem marcador nas duas pontas e fica.
fn strip_emphasis(w: &str) -> &str {
    let marker = |c: char| c == '*' || c == '_';
    let lead = w.len() - w.trim_start_matches(marker).len();
    let trail = w.len() - w.trim_end_matches(marker).len();
    let n = lead.min(trail);
    if n == 0 || 2 * n >= w.len() {
        return w;
    }
    &w[n..w.len() - n]
}

/// Arquivos conhecidos sem extensão; só contam entre crases.
const KNOWN_NAMES: &[&str] = &["Dockerfile", "Makefile", "README", "LICENSE", "Procfile", "Containerfile"];

/// Token entre crases que é um nome conhecido sem extensão (`Dockerfile`, `docker/Dockerfile`).
fn known_name(raw: &str) -> Option<String> {
    let t = raw.trim().trim_start_matches("./");
    let name = t.rsplit('/').next().unwrap_or(t);
    (KNOWN_NAMES.contains(&name) && !is_env_path(t) && !t.starts_with('/') && !t.contains('*') && chars_ok(t)).then(|| t.to_string())
}

/// Glob com cara de caminho: `*` com `/` (`tests/**`, `src/*.rs`) ou com extensão conhecida depois
/// do último `*` (`*.ts`). Só `*` (`*`, `**`, `*/`) ou `*` entre letras (`a*b`) não contam.
fn is_glob(t: &str) -> bool {
    if !t.contains('*') || t.chars().all(|c| c == '*' || c == '/') {
        return false;
    }
    let after = t.rsplit('*').next().unwrap_or("");
    t.contains('/') || (after.starts_with('.') && has_file_extension(after))
}

/// O último segmento termina numa extensão de [`FILE_EXTENSIONS`] (dotfile incluso: `.gitignore`).
fn has_file_extension(s: &str) -> bool {
    let name = s.rsplit('/').next().unwrap_or(s);
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| FILE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Caminho de ambiente (`.nants` e `.nants/...`, com ou sem `./`): só `request_environment_change`
/// o altera, então nunca vira aviso de critério.
fn is_env_path(path: &str) -> bool {
    let t = path.trim().trim_start_matches("./").trim_end_matches('/');
    t == ".nants" || t.starts_with(".nants/")
}

fn normalize(raw: &str) -> Option<String> {
    let had_slash = raw.trim().ends_with('/');
    let t = raw.trim().trim_start_matches("./").trim_end_matches('/');
    if is_env_path(t) {
        return None;
    }
    if t.is_empty() || t.len() > 200 || t.starts_with('/') || t.starts_with('-') || t.starts_with('~') {
        return None;
    }
    // `*` só em glob com cara de caminho (`*`, `**`, `*/`, `a*b` são texto).
    if t.contains("://") || !chars_ok(t) || (t.contains('*') && !is_glob(t)) {
        return None;
    }
    // IPs e versões (só dígitos e pontos, porta opcional) não são caminho.
    let numeric = t.split_once(':').map_or(t, |(h, port)| {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) { h } else { t }
    });
    if numeric.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    // Só tem cara de arquivo: extensão conhecida, glob ou pasta terminada em `/`
    // (`application/json` e `pageerror/console.error` não contam).
    (had_slash || is_glob(t) || has_file_extension(t)).then(|| t.to_string())
}

fn chars_ok(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || "/._-@+*".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, scope: &[&str], body: &str) -> Task {
        let raw = format!(
            "---\nid: \"{id}\"\nspec_id: \"s\"\ntitle: \"t\"\nstatus: \"TODO\"\ndepends_on: []\nallowed_scope: [{}]\nbranch: \"\"\nattempts: 0\nmax_attempts: 3\nlast_review_verdict: \"NONE\"\nreviewed_sha: \"\"\ntest_evidence: \"\"\nnext_action: \"\"\n---\n{body}",
            scope.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ")
        );
        Task::parse(&raw).expect("task")
    }

    #[test]
    fn warns_for_path_outside_scope() {
        let t = task(
            "T05",
            &["src/**"],
            "## Objetivo\nx\n\n## Critérios de Aceitação\n1. `.gitignore` ignora e `deps.edn` existe\n2. `src/a.rs` ok\n",
        );
        let w = scope_warnings(&[t]);
        assert!(w.contains(&"T05: critério cita `.gitignore`, fora do escopo".to_string()), "{w:?}");
        assert!(w.contains(&"T05: critério cita `deps.edn`, fora do escopo".to_string()), "{w:?}");
        assert!(w.iter().all(|x| !x.contains("src/a.rs")));
    }

    #[test]
    fn no_warning_when_in_scope_or_dir_prefix() {
        let t = task(
            "T01",
            &["src/**", "deps.edn"],
            "## Critérios de Aceitação\n`deps.edn` e `src/x/y.rs` e `src/`\n",
        );
        assert!(scope_warnings(&[t]).is_empty());
    }

    #[test]
    fn ignored_tokens_without_known_extension() {
        let t = task(
            "T02",
            &["src/**"],
            "## Critérios de Aceitação\nUsa `Vec.len` e `cargo test` e `foo()` e https://x.io/a.html e `src/*.rs`\n",
        );
        assert!(scope_warnings(&[t]).is_empty());
    }

    #[test]
    fn slash_words_without_file_extension_are_not_paths() {
        // Spec 092 T01: `pageerror/console.error` (Test 10) não é caminho.
        let text = "Sem `pageerror/console.error`, pageerror/console.error, `request/response` e input/output.";
        assert!(path_tokens(text).is_empty(), "{:?}", path_tokens(text));
        let t = task("T07", &["web/**"], &format!("## Critérios de Aceitação\n{text}\n"));
        assert!(scope_warnings(&[t]).is_empty());
    }

    #[test]
    fn markdown_emphasis_and_bare_stars_are_not_globs() {
        // Revisão da 092 T01 (F-01): ênfase e operadores não viram glob.
        let text = "Os campos **input/output** validados, *desktop/mobile* e _a/b_; `*`, `**`, `a*b`, `*/`.";
        assert!(path_tokens(text).is_empty(), "{:?}", path_tokens(text));
        let t = task("T10", &["web/**"], &format!("## Critérios de Aceitação\n{text}\n"));
        assert!(scope_warnings(&[t]).is_empty());
    }

    #[test]
    fn globs_with_slash_or_extension_count() {
        let tokens = path_tokens("`tests/**`, `src/*.rs`, `*.ts`, **docs/guia.md** e e2e/**");
        assert_eq!(tokens, vec!["tests/**", "src/*.rs", "*.ts", "docs/guia.md", "e2e/**"]);
    }

    #[test]
    fn env_files_never_warn_even_with_known_names() {
        // Revisão da 092 T01 (F-04): `.nants/Dockerfile` é o ambiente, fora do alcance do critério.
        let text = "`.nants/Dockerfile`, `./.nants/Containerfile`, `.nants/compose.yaml` e `./.nants/`";
        assert!(path_tokens(text).is_empty(), "{:?}", path_tokens(text));
        let t = task("T11", &["src/**"], &format!("## Critérios de Aceitação\n{text}\n"));
        assert!(scope_warnings(&[t]).is_empty());
    }

    #[test]
    fn media_manifests_and_known_names_count() {
        // F-03: mídia e manifestos; nomes sem extensão só entre crases.
        let tokens = path_tokens(
            "`public/sounds/acerto.mp3`, `go.mod`, `models/nave.glb`, `site.webmanifest`, `Dockerfile`, `docker/Dockerfile`, `Makefile`; solto: Dockerfile e a/README",
        );
        assert_eq!(
            tokens,
            vec!["public/sounds/acerto.mp3", "go.mod", "models/nave.glb", "site.webmanifest", "Dockerfile", "docker/Dockerfile", "Makefile"]
        );
    }

    #[test]
    fn file_extensions_globs_and_folders_still_count() {
        let t = task(
            "T08",
            &["web/**"],
            "## Critérios de Aceitação\n`scripts/serve.mjs`, src/app.ts, `public/images/x.webp`, `tests/**`, `docs/` e `web/a.tsx`\n",
        );
        let w = scope_warnings(&[t]);
        let paths: Vec<_> = ["scripts/serve.mjs", "public/images/x.webp", "tests/**", "docs", "src/app.ts"]
            .iter()
            .map(|p| format!("T08: critério cita `{p}`, fora do escopo"))
            .collect();
        assert_eq!(w, paths);
    }

    #[test]
    fn bare_path_with_extension_is_detected_once() {
        let t = task(
            "T03",
            &["web/**"],
            "## Objetivo\nEditar docs/guia.md.\n## Critérios de Aceitação\n`docs/guia.md`\n",
        );
        assert_eq!(scope_warnings(&[t]).len(), 1);
    }

    #[test]
    fn nants_in_allowed_scope_is_not_treated_as_valid() {
        let t = task("T04", &["src/**", ".nants/**"], "## Critérios de Aceitação\n`src/a.rs`\n");
        let w = scope_warnings(&[t]);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].starts_with("T04: allowed_scope inclui `.nants/**`"), "{w:?}");
        assert!(w[0].contains("request_environment_change"));
    }

    #[test]
    fn mime_and_nants_ignored_but_real_paths_warn() {
        let t = task(
            "T09",
            &["web/**"],
            "## Critérios de Aceitação\n`application/json` `text/html` `.nants` `.nants/compose.yaml` `deps.edn` `.gitignore` `src/app.clj` `src/`\n",
        );
        let w = scope_warnings(&[t]);
        let paths: Vec<_> = ["deps.edn", ".gitignore", "src/app.clj", "src"].iter().map(|p| format!("T09: critério cita `{p}`, fora do escopo")).collect();
        assert_eq!(w, paths);
    }

    #[test]
    fn ips_and_versions_are_not_paths_but_real_files_warn() {
        let t = task(
            "T12",
            &["web/**"],
            "## Critérios de Aceitação\n`0.0.0.0` `127.0.0.1` `0.0.0.0:8080` `1.5` `21.0.2` `deps.edn` `.gitignore` `src/app.clj` `v1.2/notes.md`\n",
        );
        let w = scope_warnings(&[t]);
        let paths: Vec<_> = ["deps.edn", ".gitignore", "src/app.clj", "v1.2/notes.md"]
            .iter()
            .map(|p| format!("T12: critério cita `{p}`, fora do escopo"))
            .collect();
        assert_eq!(w, paths);
    }

    // Spec 094 T02: trechos reais do Test 11 (`001-simulacao-solar-3d`), com o escopo compactado
    // (sem as pastas de artefato e sem as entradas literais das texturas).
    const TEST11_T01_BODY: &str = r##"## Objetivo
Criar o esqueleto executável da aplicação Vite + TypeScript e toda a infraestrutura necessária para que `npm test`, `npm run typecheck` e `npm run build` funcionem numa worktree limpa, usando o workspace Docker já definido pelo plano. Fixar dependências diretas em versões exatas, criar `.gitignore` e testes mínimos de unidade e browser; preparar o README para a documentação final da T05.

## Critérios de Aceitação
- [ ] `package.json` e `package-lock.json` existem; as dependências diretas são `three` 0.186.1, `@types/three` 0.186.0, `vite` 8.3.4, `typescript` 7.0.2, `vitest` 5.0.5, `@playwright/test` 1.64.1 e `prettier` 3.9.10, todas fixadas sem `^`, `~`, `*`, `latest` ou intervalos. Verificação: `node -e 'const p=require("./package.json"); for (const d of ["dependencies","devDependencies"]) for (const [n,v] of Object.entries(p[d]||{})) if (!/^[0-9]+[.][0-9]+[.][0-9]+(-[0-9A-Za-z.-]+)?$/.test(v)) throw Error(`${n}: ${v}`)'` termina com código 0.
- [ ] Os scripts `dev`, `build`, `typecheck`, `test:unit`, `test:e2e` e `test` existem; `npm test` executa Vitest e Playwright. O config do Playwright sobe Vite atual com `webServer` em `127.0.0.1:4173`, sem depender de `dist` de uma execução anterior.
- [ ] `.gitignore` ignora pelo menos `node_modules/`, `dist/`, `.vite/`, `.vitest/`, `coverage/`, `test-results/` e `playwright-report/`; as pastas de artefatos da stack estão também declaradas em `allowed_scope` para que formatter/testes possam criá-las sem sair do escopo.
- [ ] `index.html` e a aplicação inicializam com título/heading acessível “Sistema Solar 3D”; `npm run typecheck` e `npm run build` passam no workspace.
- [ ] `README.md` é criado como ponto de partida para a documentação completa da T05.
"##;

    const TEST11_T03_BODY: &str = r##"## Objetivo
Construir a cena 3D real-time com Three.js, câmera orbital/zoom, iluminação, órbitas, estrelas e todos os corpos do catálogo.

Imagens a gerar (mapas equiretangulares tileáveis, sem emendas, 1024×512, sem texto, sem logotipos, opacos, formato WebP, em `public/assets/textures/`): `public/assets/textures/sun.webp` (granulação de fotosfera), `public/assets/textures/mercury.webp` (crateras), `public/assets/textures/earth.webp` (oceanos, continentes e nuvens), `public/assets/textures/moon.webp` (regolito e mares). Se a geração de qualquer mapa não estiver disponível, produzir no caminho correspondente um SVG determinístico local de 1024×512 com extensão `.svg`; os 17 caminhos SVG alternativos estão explicitamente incluídos em `allowed_scope`.

## Critérios de Aceitação
- [ ] Para cada um dos 17 corpos, a cena carrega um mapa local equiretangular de 1024×512 em um dos formatos previstos no Objetivo (`*.webp` ou `.svg`); `sun.webp`, `earth.webp` e `moon.webp` são os primeiros carregados.
- [ ] O e2e grava um screenshot PNG de viewport sob `test-results/` com nome descritivo depois de localizar/rolar até o elemento acessível que evidencia a cena.
- [ ] `npm test`, typecheck e build continuam verdes no workspace Docker.
"##;

    const TEST11_T01_SCOPE: &[&str] =
        &[".gitignore", "package.json", "package-lock.json", "index.html", "README.md", "src/**", "tests/**", "public/**"];
    const TEST11_T03_SCOPE: &[&str] = &[
        "src/scene/**",
        "src/main.ts",
        "src/style.css",
        "public/assets/textures/**",
        "tests/e2e/scene.spec.ts",
        "playwright.config.ts",
        "test-results/**",
    ];

    #[test]
    fn test11_fixture_has_no_false_positives() {
        // Os artefatos eram candidatos (antes da 094 viravam ~30 avisos); agora nenhum aviso.
        let tokens = path_tokens(TEST11_T01_BODY);
        for dir in ["node_modules", "dist", ".vite", "coverage", "test-results", "playwright-report"] {
            assert!(tokens.contains(&dir.to_string()), "{dir} é token: {tokens:?}");
        }
        assert!(path_tokens(TEST11_T03_BODY).contains(&".svg".to_string()));
        let tasks = [task("T01", TEST11_T01_SCOPE, TEST11_T01_BODY), task("T03", TEST11_T03_SCOPE, TEST11_T03_BODY)];
        assert_eq!(scope_warnings(&tasks), Vec::<String>::new());
        assert_eq!(grouped_scope_warnings(&tasks), Vec::<String>::new());
    }

    #[test]
    fn test11_fixture_keeps_gitignore_when_out_of_scope() {
        let scope: Vec<&str> = TEST11_T01_SCOPE.iter().copied().filter(|g| *g != ".gitignore").collect();
        let w = scope_warnings(&[task("T01", &scope, TEST11_T01_BODY)]);
        assert_eq!(w, vec!["T01: objetivo cita `.gitignore`, fora do escopo".to_string()]);
    }

    #[test]
    fn extension_mentions_are_not_paths_but_dotfiles_are() {
        let t = task(
            "T20",
            &["src/**"],
            "## Critérios de Aceitação\n- Mapas com extensão `.svg`, `.webp` ou `*.png`, testes `*.test.ts`.\n- Cria `.env`, `.npmrc`, `.dockerignore` e `.gitignore`.\n",
        );
        let paths: Vec<_> =
            [".env", ".npmrc", ".dockerignore", ".gitignore"].iter().map(|p| format!("T20: critério cita `{p}`, fora do escopo")).collect();
        assert_eq!(scope_warnings(&[t]), paths);
    }

    #[test]
    fn exclusion_context_suppresses_only_dirs_and_artifacts() {
        // `.gitignore` ignora pastas: as pastas somem, o `.gitignore` segue o escopo.
        let body = "## Critérios de Aceitação\n- `.gitignore` ignora `node_modules/` e `dist/`.\n";
        let w = scope_warnings(&[task("T21", &["src/**"], body)]);
        assert_eq!(w, vec!["T21: critério cita `.gitignore`, fora do escopo".to_string()]);
        assert!(scope_warnings(&[task("T21", &["src/**", ".gitignore"], body)]).is_empty());
        // Arquivo com extensão, glob de arquivo e o arquivo de ignore nunca são suprimidos.
        let t = task(
            "T22",
            &["web/**"],
            "## Critérios de Aceitação\n- Não versionar `build/**`, `tmp/`, `cache/` nem `deps.edn`; `.dockerignore` exclude `out/*.log`.\n",
        );
        let paths: Vec<_> =
            ["deps.edn", ".dockerignore", "out/*.log"].iter().map(|p| format!("T22: critério cita `{p}`, fora do escopo")).collect();
        assert_eq!(scope_warnings(&[t]), paths);
    }

    #[test]
    fn plain_delete_or_other_sentence_still_warns() {
        // "excluir" sozinho (apagar) não é exclusão do versionamento.
        let body = "## Critérios de Aceitação\n- Excluir `src/legacy/old.ts` e `dist/` ao final.\n";
        let w = scope_warnings(&[task("T23", &["web/**"], body)]);
        let paths: Vec<_> = ["src/legacy/old.ts", "dist"].iter().map(|p| format!("T23: critério cita `{p}`, fora do escopo")).collect();
        assert_eq!(w, paths);
        // A mesma pasta citada também fora da frase de exclusão continua acusando.
        let t = task(
            "T24",
            &["src/**", ".gitignore"],
            "## Critérios de Aceitação\n- `.gitignore` ignora `dist/`. O build grava em `dist/`.\n",
        );
        assert_eq!(scope_warnings(&[t]), vec!["T24: critério cita `dist`, fora do escopo".to_string()]);
    }

    #[test]
    fn bare_names_need_cited_path_or_literal_entry() {
        // `**` sozinho nunca cobre nome solto.
        let w = scope_warnings(&[task("T25", &["src/**"], "## Critérios de Aceitação\n`deps.edn` lista a dependência.\n")]);
        assert_eq!(w, vec!["T25: critério cita `deps.edn`, fora do escopo".to_string()]);
        let w = scope_warnings(&[task("T25", &["**"], "## Critérios de Aceitação\n`deps.edn` e `Dockerfile`.\n")]);
        assert_eq!(w.len(), 2, "{w:?}");
        // Sem o caminho completo citado, `sun.webp` com `public/assets/textures/**` acusa.
        let scope = ["public/assets/textures/**"];
        let w = scope_warnings(&[task("T26", &scope, "## Critérios de Aceitação\n`sun.webp` carrega.\n")]);
        assert_eq!(w, vec!["T26: critério cita `sun.webp`, fora do escopo".to_string()]);
        // Com o caminho completo citado no Objetivo (dentro do escopo), não acusa.
        let body = "## Objetivo\nGerar `public/assets/textures/sun.webp`.\n\n## Critérios de Aceitação\n`sun.webp` carrega.\n";
        assert!(scope_warnings(&[task("T26", &scope, body)]).is_empty());
        // Caminho completo citado fora do escopo não dá cobertura.
        let body = "## Objetivo\nGerar `public/images/sun.webp`.\n\n## Critérios de Aceitação\n`sun.webp` carrega.\n";
        let w = scope_warnings(&[task("T26", &scope, body)]);
        assert_eq!(
            w,
            vec![
                "T26: objetivo cita `public/images/sun.webp`, fora do escopo".to_string(),
                "T26: critério cita `sun.webp`, fora do escopo".to_string()
            ]
        );
        // Entrada literal no `allowed_scope` cobre o nome solto.
        let w = scope_warnings(&[task("T27", &["src/**", "public/assets/textures/sun.webp"], "## Critérios de Aceitação\n`sun.webp`\n")]);
        assert!(w.is_empty(), "{w:?}");
        // Caminho com diretório segue comparado por inteiro.
        let w = scope_warnings(&[task("T28", &["public/assets/**"], "## Critérios de Aceitação\n`public/images/x.webp`\n")]);
        assert_eq!(w, vec!["T28: critério cita `public/images/x.webp`, fora do escopo".to_string()]);
    }

    #[test]
    fn label_tells_the_section_and_many_warnings_are_grouped() {
        let three = task(
            "T30",
            &["src/**"],
            "## Objetivo\nEditar `docs/a.md`.\n\n## Critérios de Aceitação\n`docs/b.md` e `docs/c.md`\n",
        );
        let expected = vec![
            "T30: objetivo cita `docs/a.md`, fora do escopo".to_string(),
            "T30: critério cita `docs/b.md`, fora do escopo".to_string(),
            "T30: critério cita `docs/c.md`, fora do escopo".to_string(),
        ];
        assert_eq!(scope_warnings(std::slice::from_ref(&three)), expected);
        assert_eq!(grouped_scope_warnings(&[three]), expected, "até 3 avisos, uma linha por caminho");
        let five = task(
            "T31",
            &["src/**", ".nants/**"],
            "## Objetivo\nEditar `docs/a.md`.\n\n## Critérios de Aceitação\n`docs/b.md`, `docs/c.md`, `docs/d.md` e `docs/e.md`\n",
        );
        let ok = task("T32", &["src/**"], "## Critérios de Aceitação\n`docs/z.md`\n");
        let w = grouped_scope_warnings(&[five, ok]);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w[0].starts_with("T31: allowed_scope inclui `.nants/**`"), "{w:?}");
        assert_eq!(w[1], "T31: 5 caminhos citados fora do escopo (`docs/a.md`, `docs/b.md`, `docs/c.md` e mais 2)");
        assert_eq!(w[2], "T32: critério cita `docs/z.md`, fora do escopo");
    }

    #[test]
    fn root_glob_covers_bare_name_but_double_star_does_not() {
        let w = scope_warnings(&[task("T40", &["*.md"], "## Critérios de Aceitação\n`README.md` atualizado.\n")]);
        assert!(w.is_empty(), "{w:?}");
        let w = scope_warnings(&[task("T40", &["*.md"], "## Critérios de Aceitação\n`deps.edn` atualizado.\n")]);
        assert_eq!(w, vec!["T40: critério cita `deps.edn`, fora do escopo".to_string()]);
        let w = scope_warnings(&[task("T40", &["**"], "## Critérios de Aceitação\n`README.md`\n")]);
        assert_eq!(w.len(), 1, "{w:?}");
        let w = scope_warnings(&[task("T40", &["docs/*.md"], "## Critérios de Aceitação\n`README.md`\n")]);
        assert_eq!(w.len(), 1, "glob com diretório não cobre nome solto: {w:?}");
    }

    #[test]
    fn exclusion_terms_without_accents_count() {
        for phrase in [
            "`dist/` fica excluido do versionamento.",
            "`dist/` fica excluida do versionamento.",
            "`dist/` e `coverage/` ficam excluidos do versionamento.",
            "`dist/` e `coverage/` ficam excluidas do versionamento.",
            "`dist/` fica fora do repositorio.",
        ] {
            let body = format!("## Critérios de Aceitação\n- {phrase}\n");
            let w = scope_warnings(&[task("T41", &["src/**"], &body)]);
            assert!(w.is_empty(), "{phrase}: {w:?}");
        }
    }
}
