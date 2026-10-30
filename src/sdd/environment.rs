//! Ambiente Docker do projeto (spec 030): arquivos `.nants/compose.yaml` e `.nants/Dockerfile*`,
//! validação de segurança do compose, resumo para a UI e hash dos arquivos aprovados.

use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_yaml::Value;

/// Pasta dos arquivos de ambiente, relativa à raiz do projeto.
pub const ENV_DIR: &str = ".nants";
/// Arquivo compose do ambiente.
pub const COMPOSE_REL: &str = ".nants/compose.yaml";
/// Serviço de trabalho obrigatório do compose.
pub const WORKSPACE_SERVICE: &str = "workspace";
/// Tamanho máximo de cada arquivo de ambiente (64 KiB).
pub const MAX_ENV_FILE_BYTES: usize = 64 * 1024;
/// Variável aceita como origem de bind mount (a worktree, resolvida pelo Nants na execução).
pub const WORKTREE_VAR: &str = "NANTS_WORKTREE";
/// Spec 129 T05: a variável sem valor padrão, como os projetos antigos a escrevem.
const WORKTREE_BARE: &str = "${NANTS_WORKTREE}";
/// Spec 129 T05: a origem do workspace com o valor padrão `..` (a raiz do projeto, relativa a
/// `.nants/`, onde fica o compose). Fora do Nants, `docker compose -f .nants/compose.yaml …` na raiz
/// funciona sem nenhuma variável; no Nants, o override sempre monta o caminho absoluto da worktree
/// (C-14), e o ambiente do subprocesso também define a variável.
pub const WORKTREE_SOURCE: &str = "${NANTS_WORKTREE:-..}";
/// Recusa de qualquer outra forma da variável (outro padrão, `-` sem `:`, `:?`).
const WORKTREE_FORM_MSG: &str = "compose.yaml: a origem da worktree é `${NANTS_WORKTREE:-..}` (o padrão `..` é a raiz do \
     projeto, relativa a .nants/compose.yaml); outra forma de ${NANTS_WORKTREE…} não é aceita";

/// Caminho de arquivo de ambiente gravável pelo Arquiteto: `.nants/compose.yaml` ou
/// `.nants/Dockerfile*` (nome simples, sem subpasta).
pub fn is_env_file(rel: &str) -> bool {
    if rel == COMPOSE_REL {
        return true;
    }
    match rel.strip_prefix(".nants/") {
        Some(name) => {
            name.starts_with("Dockerfile")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        }
        None => false,
    }
}

/// Spec 129 T05: arquivo de ambiente novo (escrito pelo Arquiteto no plano, numa mudança de
/// ambiente ou numa opção de recuperação) já ajustado e validado. No `compose.yaml`, toda
/// `${NANTS_WORKTREE}` sem valor padrão vira `${NANTS_WORKTREE:-..}`: o projeto entregue roda pelo
/// README sem nenhuma variável do Nants. Os projetos antigos não são migrados (a validação de
/// sempre, [`validate_env_file`], ainda aceita a forma sem padrão).
pub fn prepare_env_file(rel: &str, content: &str) -> Result<String, String> {
    let adjusted = if rel == COMPOSE_REL { with_worktree_default(content) } else { content.to_string() };
    validate_env_file(rel, &adjusted)?;
    Ok(adjusted)
}

/// Troca cada `${NANTS_WORKTREE}` (fora de `$$`, o cifrão literal) por [`WORKTREE_SOURCE`].
pub fn with_worktree_default(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some(after) = rest.strip_prefix("$$") {
            out.push_str("$$");
            rest = after;
        } else if let Some(after) = rest.strip_prefix(WORKTREE_BARE) {
            out.push_str(WORKTREE_SOURCE);
            rest = after;
        } else {
            out.push('$');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// Valida o conteúdo de um arquivo de ambiente. Mensagens sem eco do conteúdo (só nomes de chave
/// e de serviço).
pub fn validate_env_file(rel: &str, content: &str) -> Result<(), String> {
    if content.len() > MAX_ENV_FILE_BYTES {
        return Err("arquivo de ambiente grande demais (limite de 64 KiB)".into());
    }
    if rel == COMPOSE_REL {
        validate_compose(content)
    } else if content.trim().is_empty() {
        Err("Dockerfile vazio".into())
    } else {
        Ok(())
    }
}

type Check<T = ()> = Result<T, String>;

/// Chaves aceitas (ALLOWLIST estrutural) no topo do compose.
const TOP_KEYS: &[&str] = &["services", "volumes", "networks", "name"];
const SERVICE_KEYS: &[&str] = &[
    "image", "build", "command", "entrypoint", "working_dir", "environment", "ports", "expose",
    "depends_on", "healthcheck", "volumes", "restart", "user", "labels", "networks", "stdin_open",
    "tty", "init", "mem_limit", "cpus", "shm_size",
];
const BUILD_KEYS: &[&str] = &["context", "dockerfile", "args", "target"];
const TOP_VOLUME_KEYS: &[&str] = &["name", "labels"];
const TOP_NETWORK_KEYS: &[&str] = &["labels", "internal"];
const LONG_VOLUME_KEYS: &[&str] = &["type", "source", "target", "read_only"];

fn key(s: &str) -> Value {
    Value::String(s.to_string())
}

/// Recusa chaves fora da lista em `map`; `ctx` descreve o local na mensagem.
fn only_keys(map: &serde_yaml::Mapping, allowed: &[&str], ctx: &str) -> Check {
    for k in map.keys() {
        match k.as_str() {
            Some(name) if allowed.contains(&name) => {}
            Some(name) => {
                return Err(format!("compose.yaml: chave `{name}` não permitida em {ctx}"));
            }
            None => return Err(format!("compose.yaml: chave não textual em {ctx}")),
        }
    }
    Ok(())
}

/// `null` ou mapeamento vazio/permitido.
fn opt_map<'a>(v: &'a Value, ctx: &str) -> Check<Option<&'a serde_yaml::Mapping>> {
    match v {
        Value::Null => Ok(None),
        Value::Mapping(m) => Ok(Some(m)),
        _ => Err(format!("compose.yaml: {ctx} inválido")),
    }
}

/// Âncoras/aliases YAML (`&x`, `*x`) em posição de token, por varredura sintática simples.
fn has_anchor_or_alias(text: &str) -> bool {
    let mut prev = ' ';
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if (c == '&' || c == '*')
            && (prev.is_whitespace() || matches!(prev, '[' | ',' | '{'))
            && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric() || *n == '_' || *n == '-')
        {
            return true;
        }
        prev = c;
    }
    false
}

/// Alguma string da árvore usa `${NANTS_WORKTREE}` (após remover `$$`).
fn mentions_worktree_var(v: &Value) -> bool {
    match v {
        Value::String(s) => s.replace("$$", "").contains("${"),
        Value::Mapping(m) => m.iter().any(|(k, v)| mentions_worktree_var(k) || mentions_worktree_var(v)),
        Value::Sequence(s) => s.iter().any(mentions_worktree_var),
        _ => false,
    }
}

/// `environment`/`build.args`: toda entrada precisa de valor literal (item de lista com `=`,
/// mapa sem valor null), senão o Compose herdaria a variável do host.
fn check_valued(v: &Value, ctx: &str) -> Check {
    let missing = || format!("compose.yaml: entrada sem valor em {ctx} (herdaria o ambiente do host)");
    match v {
        Value::Sequence(items) => {
            for it in items {
                if !it.as_str().is_some_and(|s| s.contains('=')) {
                    return Err(missing());
                }
            }
            Ok(())
        }
        Value::Mapping(m) => {
            if m.values().any(Value::is_null) {
                return Err(missing());
            }
            Ok(())
        }
        Value::Null => Ok(()),
        _ => Err(format!("compose.yaml: {ctx} inválido")),
    }
}

/// Recusa a chave de merge `<<` em qualquer nível.
fn has_merge_key(v: &Value) -> bool {
    match v {
        Value::Mapping(m) => m.iter().any(|(k, v)| k.as_str() == Some("<<") || has_merge_key(v)),
        Value::Sequence(s) => s.iter().any(has_merge_key),
        _ => false,
    }
}

/// Valida o `compose.yaml` por ALLOWLIST: só chaves conhecidas e seguras, serviço `workspace`,
/// binds dentro do projeto e volumes/redes declarados. Sem âncoras, aliases nem `<<`.
pub fn validate_compose(text: &str) -> Check {
    if text.len() > MAX_ENV_FILE_BYTES {
        return Err("compose.yaml grande demais (limite de 64 KiB)".into());
    }
    if text.contains("docker.sock") {
        return Err("compose.yaml não pode expor o docker.sock".into());
    }
    // Interpolação herdaria o ambiente do host (segredos): só `$$` (escape), `${NANTS_WORKTREE}` e
    // `${NANTS_WORKTREE:-..}` (spec 129 T05).
    let probe = text.replace("$$", "").replace(WORKTREE_SOURCE, "").replace(WORKTREE_BARE, "");
    if probe.contains("${NANTS_WORKTREE") {
        return Err(WORKTREE_FORM_MSG.into());
    }
    if probe.contains('$') {
        return Err("compose.yaml não pode interpolar variáveis ($); use $$ para um cifrão literal".into());
    }
    if has_anchor_or_alias(text) {
        return Err("compose.yaml não pode usar âncoras nem aliases YAML".into());
    }
    let doc: Value = serde_yaml::from_str(text).map_err(|_| "compose.yaml não é um YAML válido".to_string())?;
    if has_merge_key(&doc) {
        return Err("compose.yaml não pode usar a chave de merge `<<`".into());
    }
    let top = doc.as_mapping().ok_or("compose.yaml inválido")?;
    only_keys(top, TOP_KEYS, "nível raiz")?;
    for (k, v) in top {
        if k.as_str() != Some("services") && mentions_worktree_var(v) {
            return Err("compose.yaml: ${NANTS_WORKTREE} só é permitido na origem de bind".into());
        }
    }
    let services = top
        .get(key("services"))
        .and_then(Value::as_mapping)
        .ok_or("compose.yaml sem a seção services")?;
    if !services.get(key(WORKSPACE_SERVICE)).is_some_and(Value::is_mapping) {
        return Err("compose.yaml precisa do serviço `workspace`".into());
    }
    if let Some(name) = top.get(key("name")) {
        if !name.is_string() {
            return Err("compose.yaml: `name` inválido".into());
        }
    }
    let mut volumes: Vec<String> = Vec::new();
    if let Some(v) = top.get(key("volumes")) {
        for (name, def) in opt_map(v, "volumes")?.into_iter().flatten() {
            let name = name.as_str().ok_or("compose.yaml: volume sem nome textual")?;
            if let Some(m) = opt_map(def, &format!("volume `{name}`"))? {
                only_keys(m, TOP_VOLUME_KEYS, &format!("volume `{name}`"))?;
            }
            volumes.push(name.to_string());
        }
    }
    let mut networks: Vec<String> = vec!["default".into()];
    if let Some(v) = top.get(key("networks")) {
        for (name, def) in opt_map(v, "networks")?.into_iter().flatten() {
            let name = name.as_str().ok_or("compose.yaml: rede sem nome textual")?;
            if let Some(m) = opt_map(def, &format!("rede `{name}`"))? {
                only_keys(m, TOP_NETWORK_KEYS, &format!("rede `{name}`"))?;
            }
            networks.push(name.to_string());
        }
    }
    for (name, service) in services {
        let name = name.as_str().ok_or("compose.yaml: serviço sem nome textual")?;
        check_service(name, service, &volumes, &networks)?;
    }
    Ok(())
}

fn check_service(name: &str, service: &Value, volumes: &[String], networks: &[String]) -> Check {
    let ctx = format!("serviço `{name}`");
    let map = service.as_mapping().ok_or_else(|| format!("compose.yaml: {ctx} inválido"))?;
    only_keys(map, SERVICE_KEYS, &ctx)?;
    for (k, v) in map {
        if k.as_str() != Some("volumes") && mentions_worktree_var(v) {
            return Err(format!("compose.yaml: ${{NANTS_WORKTREE}} só é permitido na origem de bind ({ctx})"));
        }
    }
    if let Some(env) = map.get(key("environment")) {
        check_valued(env, &format!("environment do {ctx}"))?;
    }
    if let Some(list) = map.get(key("volumes")) {
        let list = list.as_sequence().ok_or_else(|| format!("compose.yaml: volumes do {ctx} inválidos"))?;
        for vol in list {
            check_volume(name, vol, volumes)?;
        }
    }
    if let Some(n) = map.get(key("networks")) {
        let names: Vec<&str> = match n {
            Value::Sequence(s) => s.iter().filter_map(Value::as_str).collect(),
            Value::Mapping(m) => m.keys().filter_map(Value::as_str).collect(),
            _ => return Err(format!("compose.yaml: networks do {ctx} inválidas")),
        };
        for net in names {
            if !networks.iter().any(|d| d == net) {
                return Err(format!("compose.yaml: rede `{net}` do {ctx} não declarada no topo"));
            }
        }
    }
    match map.get(key("build")) {
        None => {}
        Some(Value::String(ctxdir)) => {
            walk(0, ctxdir)?;
            check_context(ctxdir)?;
        }
        Some(Value::Mapping(b)) => {
            only_keys(b, BUILD_KEYS, &format!("build do {ctx}"))?;
            if let Some(args) = b.get(key("args")) {
                check_valued(args, &format!("build.args do {ctx}"))?;
            }
            let depth = match b.get(key("context")) {
                Some(c) => walk(0, c.as_str().ok_or("compose.yaml: build.context inválido")?)?,
                None => 0,
            };
            if let Some(c) = b.get(key("context")).and_then(Value::as_str) {
                check_context(c)?;
            }
            if let Some(df) = b.get(key("dockerfile")) {
                let df = df.as_str().ok_or("compose.yaml: build.dockerfile inválido")?;
                if df.starts_with('/') {
                    return Err(BIND_MSG.into());
                }
                walk(depth, df)?;
                // O Dockerfile (contexto + caminho) também precisa ficar dentro de `.nants/`.
                let c = b.get(key("context")).and_then(Value::as_str).unwrap_or(".");
                check_context(&format!("{c}/{df}"))?;
            }
        }
        Some(_) => return Err(format!("compose.yaml: build do {ctx} inválido")),
    }
    Ok(())
}

/// Normaliza `base/rel` (partes relativas a `.nants/`); `None` se sobe acima de `.nants/`.
fn resolve_in_nants(parts: &[&str]) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for part in parts.iter().flat_map(|p| p.split('/')) {
        match part.trim() {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            seg => out.push(seg.to_string()),
        }
    }
    Some(out)
}

/// Spec 061: confere, antes de pedir aprovação, que `build.context` (relativo a `.nants/`, onde
/// fica o compose) existe e fica dentro de `.nants/`, e que o `dockerfile` (relativo ao context)
/// existe na worktree ou na própria proposta. `proposed` = (caminho relativo, conteúdo).
pub fn check_build_paths(root: &Path, proposed: &[(String, String)]) -> Check {
    let Some((_, text)) = proposed.iter().find(|(k, _)| k == COMPOSE_REL) else {
        return Ok(());
    };
    let Ok(doc) = serde_yaml::from_str::<Value>(text) else {
        return Ok(());
    };
    let Some(services) = doc.get("services").and_then(Value::as_mapping) else {
        return Ok(());
    };
    for (name, svc) in services {
        let name = name.as_str().unwrap_or("?");
        let (ctx, df) = match svc.get("build") {
            Some(Value::String(c)) => (c.as_str(), None),
            Some(Value::Mapping(b)) => (
                b.get(key("context")).and_then(Value::as_str).unwrap_or("."),
                b.get(key("dockerfile")).and_then(Value::as_str),
            ),
            _ => continue,
        };
        let Some(dir) = resolve_in_nants(&[ctx]) else {
            return Err(format!(
                "build.context '{ctx}' do serviço {name} sai de .nants/; os caminhos do compose são relativos a \
                 .nants/compose.yaml: use context: . (ou uma subpasta existente de .nants/)"
            ));
        };
        let dir_path = dir.iter().fold(root.join(ENV_DIR), |p, seg| p.join(seg));
        if !dir_path.is_dir() {
            let shown = if dir.is_empty() { ".nants".to_string() } else { format!(".nants/{}", dir.join("/")) };
            return Err(format!(
                "build.context '{ctx}' do serviço {name} resolve para {shown}, que não existe; os caminhos do \
                 compose são relativos a .nants/compose.yaml: use context: . (e dockerfile: Dockerfile)"
            ));
        }
        if let Some(df) = df {
            let Some(file) = resolve_in_nants(&[ctx, df]) else {
                return Err(format!("build.dockerfile '{df}' do serviço {name} sai de .nants/"));
            };
            let rel = format!("{ENV_DIR}/{}", file.join("/"));
            let in_proposal = proposed.iter().any(|(k, _)| *k == rel);
            if !in_proposal && !root.join(&rel).is_file() {
                return Err(format!(
                    "build.dockerfile '{df}' do serviço {name} resolve para {rel}, que não existe nem faz \
                     parte da proposta; caminhos são relativos ao context (ex.: dockerfile: Dockerfile)"
                ));
            }
        }
    }
    Ok(())
}

/// O contexto de build precisa ficar dentro de `.nants/` (nunca sobe acima dele, em nenhum passo):
/// só o que está lá entra no hash aprovado, então nada fora dele pode ser copiado para a imagem.
fn check_context(path: &str) -> Check {
    let mut depth = 0i32;
    for part in path.trim().split('/') {
        match part {
            "" | "." => {}
            ".." => depth -= 1,
            _ => depth += 1,
        }
        if depth < 0 {
            return Err("compose.yaml: build.context deve ficar dentro de .nants/".into());
        }
    }
    Ok(())
}

fn check_volume(service: &str, vol: &Value, declared: &[String]) -> Check {
    let ctx = format!("serviço `{service}`");
    match vol {
        Value::String(s) => {
            let Some((source, _)) = split_short_volume(s) else {
                return Err(format!("compose.yaml: volume anônimo não permitido no {ctx}"));
            };
            check_source(source, declared)
        }
        Value::Mapping(m) => {
            only_keys(m, LONG_VOLUME_KEYS, &format!("volume do {ctx}"))?;
            let kind = m.get(key("type")).and_then(Value::as_str);
            let source = m
                .get(key("source"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("compose.yaml: volume do {ctx} sem source"))?;
            match kind {
                Some("bind") => walk_source(source),
                Some("volume") => {
                    if declared.iter().any(|d| d == source) {
                        Ok(())
                    } else {
                        Err(format!("compose.yaml: volume `{source}` não declarado no topo"))
                    }
                }
                _ => Err(format!("compose.yaml: type de volume do {ctx} deve ser bind ou volume")),
            }
        }
        _ => Err(format!("compose.yaml: volumes do {ctx} inválidos")),
    }
}

/// Sintaxe curta `origem:destino[:modo]` em (origem, resto); a origem `${…}` pode ter `:` dentro
/// (`${NANTS_WORKTREE:-..}`). `None` sem destino (volume anônimo).
fn split_short_volume(s: &str) -> Option<(&str, &str)> {
    let skip = if s.trim_start().starts_with("${") { s.find('}').map_or(0, |i| i + 1) } else { 0 };
    let at = skip + s[skip..].find(':')?;
    Some((&s[..at], &s[at + 1..]))
}

/// Origem na sintaxe curta: bind relativo dentro do projeto ou volume nomeado declarado.
fn check_source(source: &str, declared: &[String]) -> Check {
    let is_path = source.starts_with('.') || source.contains('/') || source.contains('$') || source.starts_with('~');
    if is_path {
        walk_source(source)
    } else if declared.iter().any(|d| d == source) {
        Ok(())
    } else {
        Err(format!("compose.yaml: volume `{source}` não declarado no topo"))
    }
}

const BIND_MSG: &str = "bind mount fora do diretório do projeto no compose.yaml";

/// Origem de bind: relativa ao projeto (o compose fica em `.nants/`, então `..` é a raiz) ou
/// `${NANTS_WORKTREE}[/sub]` e `${NANTS_WORKTREE:-..}[/sub]`.
fn walk_source(source: &str) -> Check {
    let s = source.trim();
    if s.is_empty() || s.starts_with('/') || s.starts_with('~') || s.contains('\\') {
        return Err(BIND_MSG.into());
    }
    match s.strip_prefix("${").and_then(|r| r.split_once('}')) {
        // Com a variável, o resto é relativo à RAIZ do projeto (profundidade -1): vazio ou `/sub`,
        // e nunca sobe acima da raiz (spec 129 T05, F-01).
        Some((var, rest)) if var == WORKTREE_VAR || var == "NANTS_WORKTREE:-.." => {
            if !rest.is_empty() && !rest.starts_with('/') {
                return Err(BIND_MSG.into());
            }
            walk(-1, rest.trim_start_matches('/')).map(|_| ())
        }
        _ if s.contains('$') => Err(BIND_MSG.into()),
        _ => walk(0, s).map(|_| ()),
    }
}

/// Caminha por `path` a partir da profundidade `start` (relativa a `.nants/`); nunca pode ficar
/// acima da raiz do projeto (profundidade < -1). Devolve a profundidade final.
fn walk(start: i32, path: &str) -> Check<i32> {
    let s = path.trim();
    if s.starts_with('/') || s.starts_with('~') || s.contains('\\') || s.contains('$') {
        return Err(BIND_MSG.into());
    }
    let mut depth = start;
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => depth -= 1,
            _ => depth += 1,
        }
        if depth < -1 {
            return Err(BIND_MSG.into());
        }
    }
    Ok(depth)
}

/// Arquivos de ambiente do projeto (`caminho`, `conteúdo`), em ordem de caminho. Só arquivos
/// regulares (sem symlink) e legíveis; ausente → vazio.
pub fn read_env_files(root: &Path) -> Vec<(String, String)> {
    let dir = root.join(ENV_DIR);
    let Ok(meta) = fs::symlink_metadata(&dir) else { return Vec::new() };
    if !meta.is_dir() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&dir) else { return Vec::new() };
    let mut out: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let rel = format!("{ENV_DIR}/{name}");
            if !is_env_file(&rel) {
                return None;
            }
            let m = fs::symlink_metadata(e.path()).ok()?;
            if !m.is_file() || m.len() as usize > MAX_ENV_FILE_BYTES {
                return None;
            }
            Some((rel, fs::read_to_string(e.path()).ok()?))
        })
        .collect();
    out.sort();
    out
}

/// Limites da varredura de `.nants/` para o hash (contexto de build).
const MAX_HASH_FILES: usize = 512;
const MAX_HASH_BYTES: u64 = 32 * 1024 * 1024;

/// Todos os arquivos de `.nants/` (recursivo, em ordem de caminho) com o conteúdo em bytes: o
/// contexto de build é restrito a `.nants/` (ver `check_context`), então tudo que o Dockerfile pode
/// copiar entra no hash aprovado. Symlink ou arquivo especial vira entrada marcada (muda o hash e
/// é recusado por [`validate_and_hash`]). `None` se passar dos limites.
fn hashed_files(root: &Path) -> Option<Vec<(String, Vec<u8>, bool)>> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let base = root.join(ENV_DIR);
    match fs::symlink_metadata(&base) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Some(vec![(ENV_DIR.to_string(), Vec::new(), false)]),
        Err(_) => return Some(out),
    }
    let mut stack = vec![(base, ENV_DIR.to_string())];
    while let Some((dir, rel)) = stack.pop() {
        for entry in fs::read_dir(&dir).ok()?.flatten() {
            let name = entry.file_name().into_string().ok()?;
            let child = format!("{rel}/{name}");
            let meta = fs::symlink_metadata(entry.path()).ok()?;
            if meta.is_dir() {
                stack.push((entry.path(), child));
            } else if meta.is_file() {
                total += meta.len();
                if total > MAX_HASH_BYTES || out.len() >= MAX_HASH_FILES {
                    return None;
                }
                out.push((child, fs::read(entry.path()).ok()?, true));
            } else {
                out.push((child, Vec::new(), false));
            }
        }
    }
    out.sort();
    Some(out)
}

/// Hash (SHA-256 hex) dos arquivos de ambiente (todo o conteúdo de `.nants/`); `None` se não
/// houver `compose.yaml` ou se `.nants/` passar dos limites.
pub fn env_hash(root: &Path) -> Option<String> {
    let files = hashed_files(root)?;
    if !files.iter().any(|(rel, _, _)| rel == COMPOSE_REL) {
        return None;
    }
    let mut data = Vec::new();
    for (rel, content, regular) in &files {
        let rel = if *regular { rel.clone() } else { format!("{rel}\u{1}special") };
        data.extend_from_slice(format!("{rel}\0{}\0", content.len()).as_bytes());
        data.extend_from_slice(content);
        data.push(0);
    }
    Some(sha256_hex(&data))
}

/// Valida todos os arquivos de ambiente do projeto e devolve o hash (aprovação do plano).
pub fn validate_and_hash(root: &Path) -> Result<String, String> {
    const MISSING: &str = "ambiente docker sem .nants/compose.yaml";
    let files = read_env_files(root);
    if !files.iter().any(|(rel, _)| rel == COMPOSE_REL) {
        return Err(MISSING.into());
    }
    for (rel, content) in &files {
        validate_env_file(rel, content)?;
    }
    match hashed_files(root) {
        Some(all) if all.iter().all(|(_, _, regular)| *regular) => {}
        Some(_) => return Err("link simbólico ou arquivo especial em .nants/".into()),
        None => return Err("conteúdo de .nants/ grande demais".into()),
    }
    env_hash(root).ok_or_else(|| MISSING.to_string())
}

/// Limite do diff exibido por arquivo (bytes de texto).
pub const MAX_DIFF_BYTES: usize = 8 * 1024;
const DIFF_CONTEXT: usize = 3;
/// Acima disto (células da tabela LCS) o diff vira "tudo removido, tudo adicionado".
const MAX_LCS_CELLS: usize = 4_000_000;

/// Diff unificado (texto puro) de `old` → `new` para exibição ao humano, limitado a
/// [`MAX_DIFF_BYTES`]. `old` vazio = arquivo novo. Sem diferença → texto vazio.
pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // ops: ' ' igual, '-' removida, '+' adicionada
    let mut ops: Vec<(char, &str)> = Vec::new();
    if (a.len() + 1) * (b.len() + 1) > MAX_LCS_CELLS {
        ops.extend(a.iter().map(|l| ('-', *l)));
        ops.extend(b.iter().map(|l| ('+', *l)));
    } else {
        let w = b.len() + 1;
        let mut t = vec![0u32; (a.len() + 1) * w];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                t[i * w + j] = if a[i] == b[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                ops.push((' ', a[i]));
                i += 1;
                j += 1;
            } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
                ops.push(('-', a[i]));
                i += 1;
            } else {
                ops.push(('+', b[j]));
                j += 1;
            }
        }
        ops.extend(a[i..].iter().map(|l| ('-', *l)));
        ops.extend(b[j..].iter().map(|l| ('+', *l)));
    }
    let keep: Vec<bool> = (0..ops.len())
        .map(|i| {
            let lo = i.saturating_sub(DIFF_CONTEXT);
            let hi = (i + DIFF_CONTEXT + 1).min(ops.len());
            ops[lo..hi].iter().any(|(c, _)| *c != ' ')
        })
        .collect();
    let mut out = format!("--- {}\n+++ {path}\n", if old.is_empty() { "/dev/null" } else { path });
    let mut gap = false;
    for (i, (c, line)) in ops.iter().enumerate() {
        if !keep[i] {
            gap = true;
            continue;
        }
        if gap {
            out.push_str("@@\n");
            gap = false;
        }
        out.push(*c);
        out.push_str(line);
        out.push('\n');
        if out.len() > MAX_DIFF_BYTES {
            let mut end = MAX_DIFF_BYTES;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push_str("\n[diff truncado]\n");
            break;
        }
    }
    out
}

static APPROVING: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

fn canon(root: &Path) -> std::path::PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Marca o projeto como "em aprovação de ambiente": enquanto o guard existir, o Arquiteto não
/// pode gravar arquivos de ambiente (ver [`approval_active`]).
pub struct ApprovalGuard(std::path::PathBuf);

pub fn begin_approval(root: &Path) -> ApprovalGuard {
    let path = canon(root);
    APPROVING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(path.clone());
    ApprovalGuard(path)
}

impl Drop for ApprovalGuard {
    fn drop(&mut self) {
        let mut list = APPROVING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(i) = list.iter().position(|p| *p == self.0) {
            list.remove(i);
        }
    }
}

/// `true` enquanto uma aprovação de ambiente está em curso neste projeto.
pub fn approval_active(root: &Path) -> bool {
    let path = canon(root);
    APPROVING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&path)
}

/// Arquivo de ambiente exibido no plano.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvFileView {
    pub path: String,
    pub content: String,
}

/// Resumo do ambiente para o PlanCard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvSummary {
    pub services: Vec<String>,
    /// Imagem base: `image` do `workspace` ou `FROM` do primeiro Dockerfile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_image: Option<String>,
    pub files: Vec<EnvFileView>,
}

/// Resume o ambiente do projeto; `None` sem `compose.yaml` interpretável.
pub fn summarize(root: &Path) -> Option<EnvSummary> {
    let files = read_env_files(root);
    let compose = files.iter().find(|(rel, _)| rel == COMPOSE_REL)?;
    let doc: Value = serde_yaml::from_str(&compose.1).ok()?;
    let services = doc.get("services")?.as_mapping()?;
    let names: Vec<String> = services.keys().filter_map(|k| k.as_str().map(str::to_string)).collect();
    let mut base_image = services
        .get(Value::String(WORKSPACE_SERVICE.into()))
        .and_then(|w| w.get("image"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if base_image.is_none() {
        base_image = files.iter().filter(|(rel, _)| rel != COMPOSE_REL).find_map(|(_, c)| {
            c.lines()
                .find_map(|l| l.trim().strip_prefix("FROM "))
                .and_then(|f| f.split_whitespace().next())
                .map(str::to_string)
        });
    }
    Some(EnvSummary {
        services: names,
        base_image,
        files: files.into_iter().map(|(path, content)| EnvFileView { path, content }).collect(),
    })
}

/// SHA-256 (FIPS 180-4), hex minúsculo.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, b) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = "services:\n  workspace:\n    image: mcr.microsoft.com/dotnet/sdk:8.0\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n      - ../:/src\n      - data:/data\n  db:\n    image: postgres:16\nvolumes:\n  data:\n";

    const REALISTIC: &str = "name: shop\nservices:\n  workspace:\n    build:\n      context: .\n      dockerfile: Dockerfile\n    working_dir: /work\n    command: sleep infinity\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n    depends_on:\n      db:\n        condition: service_healthy\n      mq:\n        condition: service_healthy\n    networks: [backend]\n  api:\n    image: mcr.microsoft.com/dotnet/aspnet:8.0\n    ports: [\"8080:8080\"]\n    environment:\n      ConnectionStrings__Db: Host=db\n    networks: [backend]\n  orders:\n    image: mcr.microsoft.com/dotnet/aspnet:8.0\n    expose: [\"8081\"]\n    networks: [backend]\n  db:\n    image: postgres:16\n    environment:\n      POSTGRES_PASSWORD: dev\n    volumes:\n      - pgdata:/var/lib/postgresql/data\n    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -U postgres\"]\n      interval: 5s\n      retries: 10\n    networks: [backend]\n  mq:\n    image: rabbitmq:3-management\n    healthcheck:\n      test: [\"CMD\", \"rabbitmq-diagnostics\", \"-q\", \"ping\"]\n      interval: 5s\n      retries: 10\n    networks: [backend]\nvolumes:\n  pgdata:\nnetworks:\n  backend:\n";

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn build_paths_validation() {
        let dir = std::env::temp_dir().join(format!("nants-bp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".nants")).unwrap();
        let mk = |ctx: &str, df: &str| {
            vec![(
                COMPOSE_REL.to_string(),
                format!("services:\n  workspace:\n    build:\n      context: {ctx}\n      dockerfile: {df}\n"),
            )]
        };
        let dockerfile = (".nants/Dockerfile".to_string(), "FROM x".to_string());
        let mut ok = mk(".", "Dockerfile");
        assert!(check_build_paths(&dir, &ok).is_err());
        ok.push(dockerfile.clone());
        assert!(check_build_paths(&dir, &ok).is_ok());
        let dup = check_build_paths(&dir, &mk(".nants", "Dockerfile")).unwrap_err();
        assert!(dup.contains(".nants/.nants") && dup.contains("context: ."));
        let up = check_build_paths(&dir, &mk("..", "Dockerfile")).unwrap_err();
        assert!(up.contains("sai de .nants/"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn compose_accepts_valid() {
        assert!(validate_compose(OK).is_ok());
        assert_eq!(validate_compose(REALISTIC), Ok(()));
    }

    /// Spec 137 T09: a rede `internal: true` (a da rodada offline) é permitida, e `network_mode`
    /// segue fora da lista (por isso a rodada offline entra pelo override do Nants).
    #[test]
    fn compose_allows_internal_networks_but_not_network_mode() {
        let internal = "services:\n  workspace:\n    image: x\n    networks: [sem-internet]\nnetworks:\n  sem-internet:\n    internal: true\n";
        assert_eq!(validate_compose(internal), Ok(()));
        assert!(!SERVICE_KEYS.contains(&"network_mode"));
        let none = "services:\n  workspace:\n    image: x\n    network_mode: none\n";
        assert!(validate_compose(none).expect_err("network_mode").contains("network_mode"));
    }

    #[test]
    fn compose_allowlist_rejects_each_vector_with_key_and_service() {
        let svc = |extra: &str| format!("services:\n  workspace:\n    image: x\n{extra}");
        let vectors: Vec<(&str, String)> = vec![
            ("cap_add", svc("    cap_add: [SYS_ADMIN]\n")),
            ("devices", svc("    devices: [\"/dev/sda:/dev/sda\"]\n")),
            ("pid", svc("    pid: host\n")),
            ("ipc", svc("    ipc: host\n")),
            ("userns_mode", svc("    userns_mode: host\n")),
            ("security_opt", svc("    security_opt: [\"seccomp=unconfined\"]\n")),
            ("cgroup", svc("    cgroup: host\n")),
            ("cgroup_parent", svc("    cgroup_parent: x\n")),
            ("volumes_from", svc("    volumes_from: [db]\n")),
            ("extends", svc("    extends:\n      file: other.yml\n      service: a\n")),
            ("env_file", svc("    env_file: [/etc/passwd]\n")),
            ("privileged", svc("    privileged: true\n")),
            ("network_mode", svc("    network_mode: host\n")),
            ("container_name", svc("    container_name: x\n")),
            ("include", format!("include: [other.yml]\n{}", svc(""))),
            ("configs", format!("{}configs:\n  c:\n    file: /etc/shadow\n", svc(""))),
            ("additional_contexts", svc("    build:\n      context: .\n      additional_contexts:\n        a: /etc\n")),
            ("driver_opts", format!("{}volumes:\n  v:\n    driver_opts:\n      type: none\n      o: bind\n      device: /etc\n", svc("    volumes: [\"v:/d\"]\n"))),
            ("external", format!("{}volumes:\n  v:\n    external: true\n", svc("    volumes: [\"v:/d\"]\n"))),
            ("driver_opts", format!("{}networks:\n  n:\n    driver_opts:\n      x: y\n", svc(""))),
        ];
        for (k, text) in vectors {
            let err = validate_compose(&text).expect_err(k);
            assert!(err.contains(k), "{k}: {err}");
        }
        let err = validate_compose(&svc("    cap_add: [X]\n")).expect_err("cap");
        assert!(err.contains("workspace"), "{err}");
    }

    #[test]
    fn compose_rejects_merge_anchors_and_undeclared_refs() {
        let merge = "services:\n  workspace:\n    image: x\n    <<:\n      privileged: true\n";
        assert!(validate_compose(merge).is_err());
        let anchor = "x: &a\n  image: x\nservices:\n  workspace: *a\n";
        assert!(validate_compose(anchor).is_err());
        let alias = "services:\n  workspace:\n    image: &i x\n  b:\n    image: *i\n";
        assert!(validate_compose(alias).is_err());
        let undeclared_vol = "services:\n  workspace:\n    image: x\n    volumes: [\"v:/d\"]\n";
        assert!(validate_compose(undeclared_vol).is_err());
        let undeclared_net = "services:\n  workspace:\n    image: x\n    networks: [n]\n";
        assert!(validate_compose(undeclared_net).is_err());
        let anon = "services:\n  workspace:\n    image: x\n    volumes: [\"/d\"]\n";
        assert!(validate_compose(anon).is_err());
        let df = "services:\n  workspace:\n    build:\n      context: .\n      dockerfile: ../../x\n";
        assert!(validate_compose(df).is_err());
    }

    #[test]
    fn compose_rejects_interpolation_and_valueless_entries() {
        let ws = |extra: &str| format!("services:\n  workspace:\n    image: x\n{extra}");
        let bad = [
            ws("    environment:\n      K: ${AWS_KEY}\n"),
            ws("    command: echo ${AWS_KEY}\n"),
            ws("    build:\n      context: .\n      args:\n        K: ${AWS_KEY}\n"),
            ws("    command: echo $HOME\n"),
            ws("    environment:\n      K: ${X:-y}\n"),
            ws("    environment:\n      - AWS_KEY\n"),
            ws("    environment:\n      AWS_KEY:\n"),
            ws("    environment:\n      AWS_KEY: null\n"),
            ws("    build:\n      context: .\n      args:\n        - AWS_KEY\n"),
            ws("    build:\n      context: .\n      args:\n        AWS_KEY:\n"),
            ws("    environment:\n      K: ${NANTS_WORKTREE}\n"),
        ];
        for b in bad {
            assert!(validate_compose(&b).is_err(), "deveria recusar: {b}");
        }
    }

    #[test]
    fn compose_accepts_escaped_dollar_and_worktree_bind() {
        let ok = "services:\n  workspace:\n    image: x\n    volumes:\n      - ${NANTS_WORKTREE}:/work\n    environment:\n      - A=1\n  db:\n    image: postgres:16\n    healthcheck:\n      test: [\"CMD-SHELL\", \"pg_isready -U $$POSTGRES_USER\"]\n";
        assert_eq!(validate_compose(ok), Ok(()));
    }

    /// Spec 129 T05: a origem com o valor padrão `..` é aceita (curta e longa); a forma sem padrão
    /// continua aceita (projetos antigos, sem migração); outra forma da variável é recusada com motivo.
    #[test]
    fn compose_accepts_worktree_default_and_refuses_other_forms() {
        let short = |src: &str| format!("services:\n  workspace:\n    image: x\n    volumes:\n      - \"{src}:/work\"\n");
        assert_eq!(validate_compose(&short("${NANTS_WORKTREE:-..}")), Ok(()));
        assert_eq!(validate_compose(&short("${NANTS_WORKTREE:-..}/sub")), Ok(()));
        assert_eq!(validate_compose(&short("${NANTS_WORKTREE}")), Ok(()), "sem padrão: projetos antigos");
        let long = "services:\n  workspace:\n    image: x\n    volumes:\n      - type: bind\n        source: ${NANTS_WORKTREE:-..}\n        target: /work\n";
        assert_eq!(validate_compose(long), Ok(()));
        for other in ["${NANTS_WORKTREE:-/etc}", "${NANTS_WORKTREE:-.}", "${NANTS_WORKTREE-..}", "${NANTS_WORKTREE:?x}", "${NANTS_WORKTREE:-../..}"] {
            let err = validate_compose(&short(other)).expect_err(other);
            assert!(err.contains("${NANTS_WORKTREE:-..}"), "{other}: {err}");
        }
        let elsewhere = "services:\n  workspace:\n    image: x\n    environment:\n      K: ${NANTS_WORKTREE:-..}\n";
        assert!(validate_compose(elsewhere).is_err(), "só na origem de bind");
    }

    /// Spec 129 T05 (F-01): com a variável, o resto é relativo à raiz do projeto e nunca sobe acima
    /// dela, nas sintaxes curta e longa.
    #[test]
    fn worktree_source_never_climbs_above_the_project_root() {
        let short = |src: &str| format!("services:\n  workspace:\n    image: x\n    volumes:\n      - \"{src}:/work\"\n");
        let long = |src: &str| format!("services:\n  workspace:\n    image: x\n    volumes:\n      - type: bind\n        source: \"{src}\"\n        target: /work\n");
        for bad in ["${NANTS_WORKTREE:-..}/..", "${NANTS_WORKTREE}/..", "${NANTS_WORKTREE:-..}../x", "${NANTS_WORKTREE}/a/../..", "${NANTS_WORKTREE}x"] {
            assert!(validate_compose(&short(bad)).is_err(), "curta: {bad}");
            assert!(validate_compose(&long(bad)).is_err(), "longa: {bad}");
        }
        for ok in ["${NANTS_WORKTREE:-..}", "${NANTS_WORKTREE:-..}/data", "${NANTS_WORKTREE}/sub/dir", "${NANTS_WORKTREE}/a/.."] {
            assert_eq!(validate_compose(&short(ok)), Ok(()), "curta: {ok}");
            assert_eq!(validate_compose(&long(ok)), Ok(()), "longa: {ok}");
        }
    }

    /// Spec 129 T05: o arquivo novo ganha o valor padrão (fora de `$$`); o que já tem o padrão, o
    /// Dockerfile e o compose sem a variável não mudam; o resultado passa na validação.
    #[test]
    fn prepare_env_file_adds_the_worktree_default() {
        let bare = "services:\n  workspace:\n    image: x\n    volumes:\n      - \"${NANTS_WORKTREE}:/work\"\n      - ${NANTS_WORKTREE}/data:/data\n    healthcheck:\n      test: [\"CMD-SHELL\", \"echo $${NANTS_WORKTREE}\"]\n";
        let out = prepare_env_file(COMPOSE_REL, bare).expect("ajusta");
        assert!(out.contains("- \"${NANTS_WORKTREE:-..}:/work\"") && out.contains("- ${NANTS_WORKTREE:-..}/data:/data"), "{out}");
        assert!(out.contains("echo $${NANTS_WORKTREE}"), "cifrão literal intacto: {out}");
        assert!(!out.replace("$${NANTS_WORKTREE}", "").contains("${NANTS_WORKTREE}"), "{out}");
        assert_eq!(prepare_env_file(COMPOSE_REL, &out).expect("idempotente"), out);
        let plain = "services:\n  workspace:\n    image: x\n    volumes:\n      - ..:/work\n";
        assert_eq!(prepare_env_file(COMPOSE_REL, plain).expect("sem variável"), plain);
        assert_eq!(prepare_env_file(".nants/Dockerfile", "FROM x ${NANTS_WORKTREE}\n").expect("Dockerfile"), "FROM x ${NANTS_WORKTREE}\n");
        assert!(prepare_env_file(COMPOSE_REL, "services:\n  workspace:\n    image: x\n    privileged: true\n").is_err());
        assert!(prepare_env_file(COMPOSE_REL, &short_other()).is_err());
    }

    fn short_other() -> String {
        "services:\n  workspace:\n    image: x\n    volumes:\n      - ${NANTS_WORKTREE:-/}:/work\n".to_string()
    }

    #[test]
    fn approval_guard_marks_and_releases() {
        let dir = std::env::temp_dir();
        assert!(!approval_active(&dir));
        let g = begin_approval(&dir);
        assert!(approval_active(&dir));
        drop(g);
        assert!(!approval_active(&dir));
    }

    #[test]
    fn compose_rejects_forbidden() {
        let ws = |extra: &str| format!("services:\n  workspace:\n    image: x\n{extra}");
        let cases = [
            ws("    privileged: true\n"),
            ws("    network_mode: host\n"),
            ws("    volumes:\n      - /var/run/docker.sock:/var/run/docker.sock\n"),
            ws("    volumes:\n      - /etc:/etc\n"),
            ws("    volumes:\n      - ../../x:/x\n"),
            ws("    volumes:\n      - ~/.ssh:/root/.ssh\n"),
            ws("    volumes:\n      - type: bind\n        source: /home\n        target: /h\n"),
            ws("    build: /tmp\n"),
            ws("  other:\n    privileged: true\n"),
            "services:\n  db:\n    image: x\n".to_string(),
            "não é: [yaml".to_string(),
            "x".repeat(MAX_ENV_FILE_BYTES + 1),
        ];
        for c in cases {
            assert!(validate_compose(&c).is_err(), "deveria recusar: {c}");
        }
    }

    #[test]
    fn env_file_paths() {
        assert!(is_env_file(".nants/compose.yaml"));
        assert!(is_env_file(".nants/Dockerfile"));
        assert!(is_env_file(".nants/Dockerfile.dev"));
        assert!(!is_env_file(".nants/other.yaml"));
        assert!(!is_env_file(".nants/sub/Dockerfile"));
        assert!(!is_env_file("Dockerfile"));
    }

    #[test]
    fn hash_changes_with_content_and_summary() {
        let dir = std::env::temp_dir().join(format!("nants-env-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".nants")).expect("mkdir");
        assert_eq!(env_hash(&dir), None);
        fs::write(dir.join(COMPOSE_REL), OK).expect("w");
        let h1 = validate_and_hash(&dir).expect("ok");
        assert_eq!(env_hash(&dir), Some(h1.clone()));
        fs::write(dir.join(".nants/Dockerfile"), "FROM node:20\n").expect("w");
        let h2 = env_hash(&dir).expect("hash");
        assert_ne!(h1, h2);
        let s = summarize(&dir).expect("summary");
        assert_eq!(s.services, vec!["workspace", "db"]);
        assert_eq!(s.base_image.as_deref(), Some("mcr.microsoft.com/dotnet/sdk:8.0"));
        assert_eq!(s.files.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hash_covers_extra_files_and_rejects_symlinks() {
        let dir = std::env::temp_dir().join(format!("nants-env-extra-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".nants/scripts")).expect("mkdir");
        fs::write(dir.join(COMPOSE_REL), OK).expect("w");
        let h1 = validate_and_hash(&dir).expect("ok");
        fs::write(dir.join(".nants/scripts/setup.sh"), "echo a\n").expect("w");
        let h2 = validate_and_hash(&dir).expect("ok");
        assert_ne!(h1, h2, "script copiado pelo Dockerfile entra no hash");
        fs::write(dir.join(".nants/scripts/setup.sh"), "echo b\n").expect("w");
        assert_ne!(h2, env_hash(&dir).expect("hash"));
        std::os::unix::fs::symlink("/etc/passwd", dir.join(".nants/link")).expect("ln");
        assert!(validate_and_hash(&dir).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dockerfile_must_stay_inside_nants() {
        let mk = |ctx: &str, df: &str| {
            format!("services:\n  workspace:\n    build:\n      context: {ctx}\n      dockerfile: {df}\n")
        };
        for (ctx, df) in [
            (".", "../Dockerfile"),
            ("sub", "../../Dockerfile"),
            (".", "/etc/Dockerfile"),
            (".", "./../Dockerfile"),
            (".", "a/../../Dockerfile"),
            ("./sub/..", "../Dockerfile"),
        ] {
            assert!(validate_compose(&mk(ctx, df)).is_err(), "{ctx} + {df}");
        }
        assert_eq!(validate_compose(&mk(".", "Dockerfile.dev")), Ok(()));
        assert_eq!(validate_compose(&mk("sub", "../Dockerfile")), Ok(()));
    }

    #[test]
    fn build_context_must_stay_inside_nants() {
        let ok = "services:\n  workspace:\n    build:\n      context: sub\n";
        assert_eq!(validate_compose(ok), Ok(()));
        for ctx in ["..", "../..", "../x"] {
            let bad = format!("services:\n  workspace:\n    build:\n      context: {ctx}\n");
            assert!(validate_compose(&bad).is_err(), "{ctx}");
        }
        assert!(validate_compose("services:\n  workspace:\n    build: ..\n").is_err());
    }

    #[test]
    fn unified_diff_shows_changes_and_is_limited() {
        let d = unified_diff(".nants/Dockerfile", "FROM a\nRUN x\nCMD y\n", "FROM a\nRUN z\nCMD y\n");
        assert!(d.contains("-RUN x\n") && d.contains("+RUN z\n") && d.contains(" FROM a\n"), "{d}");
        assert_eq!(unified_diff("f", "a\n", "a\n"), "");
        assert!(unified_diff("f", "", "novo\n").contains("--- /dev/null"));
        let big = "linha longa\n".repeat(5000);
        let d = unified_diff("f", "", &big);
        assert!(d.len() < MAX_DIFF_BYTES + 64 && d.ends_with("[diff truncado]\n"));
    }
}
