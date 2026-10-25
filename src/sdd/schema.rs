use serde::{Deserialize, Serialize};

/// Estado de uma especificação macro (`spec.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpecStatus {
    Draft,
    AwaitingApproval,
    InProgress,
    Completed,
    Blocked,
}

/// Estado de uma tarefa (`tasks/Txx.md`), conforme BLUEPRINT §5.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskStatus {
    Todo,
    InProgress,
    InReview,
    ChangesRequested,
    Approved,
    Integrating,
    Done,
    Blocked,
}

/// Último veredito do revisor para uma tarefa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verdict {
    None,
    Approved,
    ChangesRequested,
}

/// Políticas locais do repositório (`.sdd/settings.yaml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub base_branch: String,
    pub merge_strategy: String,
    pub test_command: String,
    /// Bateria completa (spec 124 T05), opcional: com ela, o `test_command` é o nível rápido de
    /// cada tentativa e este comando roda antes da revisão e na integração. Ausente = tudo como antes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_test_command: Option<String>,
    /// Instalação das dependências (spec 124 T05), opcional: o runner a roda antes dos testes só
    /// quando o hash de `lockfiles` mudou desde a última instalação naquela pasta e ambiente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,
    /// Arquivos de trava das dependências (caminhos relativos à raiz) que decidem o `install_command`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lockfiles: Vec<String>,
    #[serde(default)]
    pub allowed_commands: Vec<String>,
    #[serde(default)]
    pub denied_commands: Vec<String>,
    /// Checagens objetivas (lint, typecheck, build) rodadas na worktree após os testes verdes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
    /// Formatadores por módulo (spec 078), rodados após cada entrega do Desenvolvedor e antes
    /// dos testes. Vazio = sem formatação automática.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formatters: Vec<Formatter>,
    /// Artefatos de diagnóstico (spec 079): globs relativos à raiz dos arquivos que a suíte grava ao
    /// falhar (ex.: `test-results/**/error-context.md`). Anexados à evidência da falha.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_artifacts: Vec<String>,
    /// Capturas de tela (spec 082 T03): globs relativos à raiz das imagens PNG/JPEG que os testes e2e
    /// gravam, no formato e com a validação de `failure_artifacts`. Revisor e Desenvolvedor as veem com
    /// `view_screenshot`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub screenshots: Vec<String>,
    /// Modo do projeto: `host` (padrão) ou `docker`.
    #[serde(default, skip_serializing_if = "Environment::is_host")]
    pub environment: Environment,
    /// Hash dos arquivos de ambiente aprovados junto com o plano (`.nants/compose.yaml`,
    /// `.nants/Dockerfile*`). Execução em modo docker recusa arquivos com hash diferente.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_hash: Option<String>,
    /// Conjunto de hashes de ambiente já aprovados pelo humano (plano ou pedido HITL), até
    /// [`MAX_APPROVED_ENV_HASHES`], FIFO e sem duplicatas. Um ambiente aprovado e depois
    /// descartado continua válido. Settings antigos (só `environment_hash`) seguem lendo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_environment_hashes: Vec<String>,
    /// Tentativas do Desenvolvedor por tarefa nova (1–10). Ausente = 5.
    #[serde(default = "default_limit", skip_serializing_if = "is_default_limit")]
    pub max_attempts: u32,
    /// Ciclos de `test_command` na mesma tentativa antes de bloquear (1–10). Ausente = 5.
    #[serde(default = "default_limit", skip_serializing_if = "is_default_limit")]
    pub max_test_cycles: u32,
    /// Modo YOLO (spec 037): aprova sozinho comandos/ambiente e faz rodadas autônomas de
    /// recuperação. Só a API do servidor altera; ausente = desligado.
    #[serde(default, skip_serializing_if = "is_false")]
    pub yolo: bool,
    /// Orçamento em bytes dos diffs de texto colados no briefing do Revisor; o excedente vira
    /// "arquivos omitidos" (pedidos com `diff_file`). Ausente = [`DEFAULT_REVIEW_DIFF_BUDGET`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_diff_budget: Option<usize>,
    /// Perfis de especialista (spec 063): `false` desliga a injeção sem apagar os arquivos
    /// (A/B). Ausente = ligado.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub skills: bool,
    /// Perguntas ao Arquiteto (`ask_architect`) por execução de agente. Ausente = 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_architect_limit: Option<u32>,
    /// Internet do Arquiteto (spec 076): busca na web, leitura de páginas e registros de pacotes
    /// no plano e na recuperação. Só a API altera; ausente = desligado.
    #[serde(default, skip_serializing_if = "is_false")]
    pub architect_internet: bool,
}

/// Orçamento padrão (bytes) dos diffs no briefing do Revisor.
pub const DEFAULT_REVIEW_DIFF_BUDGET: usize = 120 * 1024;

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Limite padrão de tentativas e ciclos de teste.
pub const DEFAULT_LIMIT: u32 = 5;
/// Faixa válida dos limites configuráveis.
pub const LIMIT_RANGE: std::ops::RangeInclusive<u32> = 1..=10;

fn default_limit() -> u32 {
    DEFAULT_LIMIT
}

fn is_default_limit(v: &u32) -> bool {
    *v == DEFAULT_LIMIT
}

/// Máximo de hashes de ambiente aprovados guardados.
pub const MAX_APPROVED_ENV_HASHES: usize = 16;

impl Settings {
    /// Hashes aceitos: o conjunto mais o `environment_hash` legado.
    pub fn approved_hashes(&self) -> Vec<String> {
        let mut out = self.approved_environment_hashes.clone();
        if let Some(h) = &self.environment_hash {
            if !out.contains(h) {
                out.push(h.clone());
            }
        }
        out
    }

    /// Acrescenta `hash` ao conjunto (FIFO, sem duplicata) e o marca como o mais recente.
    pub fn approve_environment_hash(&mut self, hash: &str) {
        let mut all = self.approved_hashes();
        all.retain(|h| h != hash);
        all.push(hash.to_string());
        let extra = all.len().saturating_sub(MAX_APPROVED_ENV_HASHES);
        all.drain(..extra);
        self.approved_environment_hashes = all;
        self.environment_hash = Some(hash.to_string());
    }
}

/// Onde o ferramental do projeto roda.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Environment {
    #[default]
    Host,
    Docker,
}

impl Environment {
    pub fn is_host(&self) -> bool {
        *self == Environment::Host
    }
}

/// Checagem objetiva definida pelo Arquiteto. `blocking=true` falhando volta ao Desenvolvedor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub blocking: bool,
}

/// Formatador de código de um módulo (spec 078). O comando só escreve (`cargo fmt`, `ruff format .`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Formatter {
    pub name: String,
    pub command: String,
    /// Diretório de execução, relativo à raiz do projeto.
    #[serde(default = "default_workdir")]
    pub workdir: String,
    /// Globs (relativos à raiz) dos arquivos que o formatador cobre.
    #[serde(default = "default_formatter_paths")]
    pub paths: Vec<String>,
}

fn default_workdir() -> String {
    ".".to_string()
}

fn default_formatter_paths() -> Vec<String> {
    vec!["**".to_string()]
}

/// Máximo de formatadores por projeto.
pub const MAX_FORMATTERS: usize = 8;
/// Máximo de globs `paths` por formatador.
const MAX_FORMATTER_PATHS: usize = 16;
/// Tamanho máximo de `workdir` e de cada glob de `paths`.
const FORMATTER_PATH_MAX_CHARS: usize = 200;

/// Normaliza um caminho relativo à raiz: sem `..`, sem caminho absoluto, sem `\`, sem controle.
/// Remove `.` e barras repetidas. Devolve os segmentos unidos por `/` (vazio = a própria raiz).
fn normalize_rel(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.chars().count() > FORMATTER_PATH_MAX_CHARS
        || raw.starts_with('/')
        || raw.contains('\\')
        || raw.chars().any(char::is_control)
    {
        return None;
    }
    let mut parts = Vec::new();
    for seg in raw.split('/') {
        match seg {
            "" | "." => {}
            ".." => return None,
            s if s.ends_with(':') && parts.is_empty() => return None, // `C:` e afins
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// Máximo de arquivos de trava (spec 124 T05).
pub const MAX_LOCKFILES: usize = 16;

/// Valida `lockfiles` (spec 124 T05): caminhos relativos à raiz, sem `..`, sem glob, sem
/// duplicata, até [`MAX_LOCKFILES`]. Os inválidos são ignorados.
pub fn validate_lockfiles(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw {
        let Some(path) = normalize_rel(item).filter(|p| !p.is_empty()) else { continue };
        if path.contains(['*', '?', '[', '{']) || out.contains(&path) {
            continue;
        }
        out.push(path);
        if out.len() == MAX_LOCKFILES {
            break;
        }
    }
    out
}

/// Comando opcional (spec 124 T05: `full_test_command`, `install_command`) com a forma do
/// `test_command`; vazio ou inválido = ausente.
pub fn valid_optional_command(raw: Option<&str>) -> Option<String> {
    raw.filter(|r| !r.trim().is_empty()).and_then(|r| check_test_command_shape(r).ok())
}

/// Normaliza `workdir`: relativo, sem `..` (vazio = `.`).
pub fn normalize_workdir(raw: &str) -> Option<String> {
    normalize_rel(raw).map(|p| if p.is_empty() { ".".to_string() } else { p })
}

/// Normaliza um glob de `paths`: relativo, sem `..`; vazio é inválido.
pub fn normalize_formatter_glob(raw: &str) -> Option<String> {
    normalize_rel(raw).filter(|p| !p.is_empty())
}

/// Valida a lista de formatadores. Itens inválidos (nome fora do padrão ou repetido, comando fora da
/// forma do `test_command`, `workdir` ou glob inválidos) são ignorados, como a spec ignora o que não
/// entende; passando de [`MAX_FORMATTERS`] valem os primeiros. `paths` vazio vira `["**"]`.
pub fn validate_formatters(formatters: &[Formatter]) -> Vec<Formatter> {
    let mut out: Vec<Formatter> = Vec::new();
    for f in formatters {
        if out.len() >= MAX_FORMATTERS {
            break;
        }
        if !check_name_valid(&f.name) || out.iter().any(|o| o.name == f.name) {
            continue;
        }
        let Ok(command) = check_test_command_shape(&f.command) else { continue };
        let Some(workdir) = normalize_workdir(&f.workdir) else { continue };
        if f.paths.len() > MAX_FORMATTER_PATHS {
            continue;
        }
        let Some(paths) = f.paths.iter().map(|g| normalize_formatter_glob(g)).collect::<Option<Vec<_>>>() else {
            continue;
        };
        let paths = if paths.is_empty() { default_formatter_paths() } else { paths };
        out.push(Formatter { name: f.name.clone(), command, workdir, paths });
    }
    out
}

/// Máximo de globs em `failure_artifacts` (spec 079).
pub const MAX_FAILURE_ARTIFACTS: usize = 8;

/// Valida `failure_artifacts` (e `screenshots`, spec 082 T03): cada glob relativo, normalizado, sem `..` nem caminho absoluto.
/// Inválidos e repetidos são ignorados um a um; passando de [`MAX_FAILURE_ARTIFACTS`] valem os primeiros.
pub fn validate_failure_artifacts(globs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for g in globs {
        if out.len() >= MAX_FAILURE_ARTIFACTS {
            break;
        }
        let Some(norm) = normalize_formatter_glob(g) else { continue };
        if !out.contains(&norm) {
            out.push(norm);
        }
    }
    out
}

/// Máximo de checagens por projeto.
pub const MAX_CHECKS: usize = 8;

/// Nome de checagem: `[a-z0-9_-]{1,32}`.
pub fn check_name_valid(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Valida a lista: até [`MAX_CHECKS`], nomes válidos e únicos, comandos com a forma do `test_command`.
/// Devolve a lista normalizada (comandos sem espaços nas bordas) ou `None` se inválida.
pub fn validate_checks(checks: &[Check]) -> Option<Vec<Check>> {
    if checks.len() > MAX_CHECKS {
        return None;
    }
    let mut out: Vec<Check> = Vec::with_capacity(checks.len());
    for c in checks {
        if !check_name_valid(&c.name) || out.iter().any(|o| o.name == c.name) {
            return None;
        }
        let command = check_test_command_shape(&c.command).ok()?;
        out.push(Check { name: c.name.clone(), command, blocking: c.blocking });
    }
    Some(out)
}

/// Tamanho máximo do comando de testes (caracteres, após `trim`).
pub const TEST_COMMAND_MAX_CHARS: usize = 200;

/// Motivo pelo qual um `test_command` não é aceito.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestCommandError {
    /// Vazio após `trim` (comando ainda não definido).
    Empty,
    /// Mais de 200 caracteres, mais de uma linha ou caractere de controle.
    Invalid,
}

/// Forma do `test_command` após `trim`: obrigatório, até 200 caracteres, uma linha, sem controle.
/// Regra única: PUT de settings, validação da spec e recusa de AWAITING_APPROVAL usam esta função.
/// Devolve o comando já sem espaços nas bordas.
pub fn check_test_command_shape(raw: &str) -> Result<String, TestCommandError> {
    let command = raw.trim();
    if command.is_empty() {
        return Err(TestCommandError::Empty);
    }
    if command.chars().count() > TEST_COMMAND_MAX_CHARS || command.chars().any(char::is_control) {
        return Err(TestCommandError::Invalid);
    }
    Ok(command.to_string())
}

/// Frontmatter de `spec.md`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecMeta {
    pub id: String,
    pub title: String,
    pub status: SpecStatus,
    pub base_branch: String,
    pub created_at: String,
    /// Comando de testes proposto pelo Arquiteto. Ausente em specs antigas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    /// Comando que sobe o sistema para teste manual (mesma forma do `test_command`). Opcional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_command: Option<String>,
    /// URL local do sistema em execução (ver [`check_run_url`]). Opcional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_url: Option<String>,
    /// Checagens (lint/typecheck/build) propostas pelo Arquiteto; adotadas no settings na aprovação
    /// se o projeto ainda não tiver checks. Inválidas são ignoradas ao interpretar a spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<Check>>,
    /// Formatadores por módulo (spec 078) propostos pelo Arquiteto. Se presente, a lista substitui a
    /// do settings na aprovação. Itens inválidos são ignorados ao interpretar a spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatters: Option<Vec<Formatter>>,
    /// Artefatos de falha (spec 079) propostos pelo Arquiteto. Se presente, a lista substitui a do
    /// settings na aprovação. Globs inválidos são ignorados ao interpretar a spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_artifacts: Option<Vec<String>>,
    /// Capturas de tela (spec 082 T03) propostas pelo Arquiteto. Se presente, a lista substitui a do
    /// settings na aprovação. Globs inválidos são ignorados ao interpretar a spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshots: Option<Vec<String>>,
    /// Imagens de referência do usuário (spec 084 T02): até 4 nomes de anexos do chat do Arquiteto
    /// que definem o visual esperado; copiadas na aprovação. Nomes inválidos são ignorados.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<Vec<String>>,
    /// Ambiente proposto pelo Arquiteto (`docker` ou `host`); valor desconhecido é ignorado.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Bateria completa proposta pelo Arquiteto (spec 124 T05; mesma forma do `test_command`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_test_command: Option<String>,
    /// Instalação das dependências proposta pelo Arquiteto (spec 124 T05; mesma forma do `test_command`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_command: Option<String>,
    /// Arquivos de trava propostos pelo Arquiteto (spec 124 T05); inválidos são ignorados.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lockfiles: Option<Vec<String>>,
}

/// Valida `run_url`: só `http://localhost`, `http://127.0.0.1` ou `http://[::1]`, porta explícita
/// 1024–65535 e caminho opcional (sem userinfo, query-only, controle ou espaço). Devolve
/// `(url, porta)`; `None` se inválida.
pub fn check_run_url(raw: &str) -> Option<(String, u16)> {
    let url = raw.trim();
    if url.is_empty() || url.chars().count() > 200 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let rest = url.strip_prefix("http://")?;
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
        Some(_) => return None,
        None => (rest, ""),
    };
    if path.contains('#') || path.contains('\\') || path.contains('@') {
        return None;
    }
    let port_text = ["localhost:", "127.0.0.1:", "[::1]:"]
        .iter()
        .find_map(|prefix| authority.strip_prefix(prefix))?;
    if port_text.is_empty() || port_text.len() > 5 || !port_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port_text.parse().ok()?;
    if port < 1024 {
        return None;
    }
    Some((url.to_string(), port))
}

impl SpecMeta {
    /// Comando de testes válido proposto pela spec (sem `trim` nas bordas); `None` se ausente,
    /// vazio ou inválido. Specs gravadas pelo parser nunca carregam forma inválida.
    pub fn valid_test_command(&self) -> Option<String> {
        self.test_command
            .as_deref()
            .and_then(|raw| check_test_command_shape(raw).ok())
    }
}

/// Especificação macro: frontmatter tipado e corpo Markdown preservado verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub frontmatter: SpecMeta,
    pub body: String,
}

/// Frontmatter de `tasks/Txx.md`. Campos desconhecidos são ignorados.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMeta {
    pub id: String,
    pub spec_id: String,
    pub title: String,
    pub status: TaskStatus,
    pub depends_on: Vec<String>,
    pub branch: String,
    pub attempts: u32,
    pub max_attempts: u32,
    /// Chave no arquivo: `last_review_verdict` (BLUEPRINT §4.2). Lê também `last_verdict`.
    #[serde(rename = "last_review_verdict", alias = "last_verdict")]
    pub last_verdict: Verdict,
    pub allowed_scope: Vec<String>,
    /// Critérios globais da spec que a tarefa entrega (`G1` = 1º item de "Critérios de Aceitação
    /// Globais"; spec 086 T07). Ids malformados são ignorados pelo parser; vazio não é gravado.
    #[serde(default, deserialize_with = "super::coverage::deserialize_covers", skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_sha: Option<String>,
    /// HEAD da worktree quando `test_command` e checagens bloqueantes passaram (spec 068).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_sha: Option<String>,
    /// Assinatura do que foi rodado em `tested_sha` (comandos + ambiente).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_signature: Option<String>,
    /// Início (RFC 3339 UTC) da execução de testes do Nants que gerou a evidência de `tested_sha`
    /// (spec 082 T03): capturas de tela mais antigas que isto não entram na revisão.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_evidence: Option<String>,
    /// Última execução dos testes na integração (base, após o squash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_run: Option<TestRun>,
    /// Motivo gravado para a pessoa, no idioma do projeto (em BLOCKED, o motivo do bloqueio).
    /// Tarefas antigas o têm em pt-BR: continua sendo exibido como está.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    /// Código estável, sem idioma, do motivo em `next_action` (spec 114 T02): é o que as máquinas
    /// leem (retomada, recuperação). Ausente em tarefas antigas e fora de BLOCKED.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action_code: Option<String>,
    /// Linhas alteradas de código (sem testes nem docs; `agents::runner::is_test_or_doc`) em `base...branch`,
    /// uma entrada por revisão de tentativa, na ordem (spec 087 T05). Vazio não é gravado.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diff_lines: Vec<u64>,
    /// Marca estável, sem idioma, de tarefas especiais criadas pelo runner (spec 107 T01): hoje só
    /// [`TASK_KIND_FINAL_FIX`]. Opcional; ausente não é gravado (tarefas comuns e antigas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Valor de `kind` da tarefa de correção criada pela verificação final (spec 107 T01).
pub const TASK_KIND_FINAL_FIX: &str = "final_fix";

/// Evidência estruturada da execução dos testes na integração.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub at: String,
    pub summary: String,
}

/// Tarefa: frontmatter tipado e corpo Markdown preservado verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub frontmatter: TaskMeta,
    pub body: String,
}

/// Finding de revisão (`F-01`, ...), descrito no corpo da tarefa.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub severity: String,
    pub location: String,
    pub description: String,
    pub recommendation: String,
    /// Gatilho concreto → resultado errado (spec 087 T05); obrigatório em HIGH. Vazio não é gravado.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub failure_scenario: String,
    /// Confiança do Revisor (spec 087 T05): `low` não bloqueia e vira observação. `high` não é gravado.
    #[serde(default, skip_serializing_if = "Confidence::is_high")]
    pub confidence: Confidence,
    /// Estado entre revisões (spec 124 T01). Na resposta do Revisor é o que ele declara (`new` ou
    /// `open`); no registro da tarefa é o que o runner apurou. Ausente nos findings antigos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<FindingState>,
    /// Revisões seguidas em que o apontamento está aberto (1 = apareceu nesta). 0 = não apurado.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub recurrence: u32,
    /// Revisão da tarefa (contagem estável entre tentativas e recuperações) em que apareceu.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_review: u32,
    /// Tentativa (`attempts + 1` na hora da revisão) em que apareceu.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_attempt: u32,
    /// Última revisão que tratou o apontamento (o viu aberto ou o deu como resolvido).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub last_review: u32,
    /// Decisão do Arquiteto sobre a contestação do Desenvolvedor (spec 124 T02). Um apontamento só é
    /// contestado uma vez: com decisão gravada, não volta à arbitragem. Ausente sem contestação.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arbitration: Option<Arbitration>,
    /// Revisões em que o Desenvolvedor respondeu `fixed` ou `test_added` e o apontamento continuou
    /// aberto (spec 126 T02). Apurado pelo runner; 0 não é gravado.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fixed_claims: u32,
    /// Revisão em que o gatilho antecipado (spec 126 T02) levou o apontamento ao Arquiteto antes de
    /// esgotar as tentativas. Vale uma vez por apontamento; 0 = nunca.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub early_review: u32,
}

/// Decisão do Arquiteto sobre um apontamento contestado (spec 124 T02).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arbitration {
    pub decision: ArbitrationDecision,
    /// Motivo dado pelo Arquiteto (texto do modelo, no idioma do projeto).
    #[serde(default)]
    pub reason: String,
    /// Revisão da tarefa antes da qual a decisão foi tomada.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub review: u32,
}

/// Opções do Arquiteto diante de uma contestação (spec 124 T02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArbitrationDecision {
    /// O apontamento continua e passa a ser obrigatório (não pode ser contestado de novo).
    Keep,
    /// O critério foi reescrito para o que o ambiente prova; o apontamento sai da cobrança.
    Adjust,
    /// Vira verificação manual documentada e sai da cobrança automática.
    Manual,
}

impl ArbitrationDecision {
    /// Código estável (`keep`, `adjust`, `manual`), o mesmo do JSON e da marca na mensagem.
    pub fn code(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Adjust => "adjust",
            Self::Manual => "manual",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim() {
            "keep" => Some(Self::Keep),
            "adjust" => Some(Self::Adjust),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// O apontamento sai da cobrança automática (ajustado ou manual).
    pub fn waives(self) -> bool {
        self != Self::Keep
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Estado de um apontamento entre revisões (spec 124 T01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingState {
    /// Apareceu nesta revisão.
    New,
    /// Já estava aberto na revisão anterior e continua.
    Open,
    /// Estava aberto e não voltou nesta revisão.
    Resolved,
}

impl FindingState {
    /// Ainda pendente (novo ou aberto).
    pub fn is_pending(self) -> bool {
        self != FindingState::Resolved
    }
}

/// Confiança do Revisor num finding (spec 087 T05); padrão `high`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    #[default]
    High,
    Low,
}

impl Confidence {
    pub fn is_high(&self) -> bool {
        *self == Confidence::High
    }
}

/// Estado SDD completo de um repositório.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdd {
    pub settings: Settings,
    /// Especificações ordenadas por nome de diretório, cada uma com suas tarefas ordenadas por nome.
    pub specs: Vec<(Spec, Vec<Task>)>,
}
