//! Capacidades do ambiente de testes (spec 124 T04): o que o ambiente aprovado consegue provar.
//!
//! Medidas de forma genérica, sem supor stack: GPU ou dispositivo de vídeo, tela ou servidor
//! gráfico, núcleos e memória, e a duração das últimas rodadas do `test_command` (medida pelo
//! runner). No Docker, a medida roda dentro do serviço `workspace` ([`PROBE_SCRIPT`], só `sh` e
//! leituras de `/dev`, `/proc` e `/sys`); no host, o próprio Nants lê as mesmas fontes.
//!
//! Ficam em [`CAPABILITIES_REL`], estado local do projeto fora do Git (`.git/info/exclude`), e se
//! atualizam sozinhas: a medida a cada execução (ou mudança do ambiente) e as durações a cada rodada.
//!
//! Os textos de [`prompt_section`] são de prompt (lidos pelos modelos), não de pessoa.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Arquivo de estado, relativo à raiz do projeto. Fica em `.sdd/specs/` (estado de trabalho que
/// não suja a base) e é excluído do Git por [`CAPABILITIES_EXCLUDE_LINE`].
pub const CAPABILITIES_REL: &str = ".sdd/specs/.capabilities.json";
/// Linha do `.git/info/exclude` do arquivo de estado.
pub const CAPABILITIES_EXCLUDE_LINE: &str = ".sdd/specs/.capabilities.json";
/// Linha do `.git/info/exclude` dos temporários da escrita atômica.
pub const CAPABILITIES_TMP_EXCLUDE_LINE: &str = ".sdd/specs/.capabilities.json.*.tmp";
/// Rodadas guardadas (as mais recentes).
pub const MAX_RUNS: usize = 5;
/// Rodadas mostradas nos prompts.
const SHOWN_RUNS: usize = 3;
/// Acima desta fração do limite, a rodada gera o aviso de orçamento de tempo (sinal, não corte).
pub const BUDGET_WARN_PERCENT: u64 = 70;
/// Tamanho máximo lido do arquivo de estado.
const MAX_STATE_BYTES: u64 = 64 * 1024;
/// Tempo máximo da medida dentro do contêiner.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Prefixo das linhas da medida, para achá-las no meio de qualquer saída do contêiner.
pub const PROBE_MARK: &str = "nants-cap:";
/// Dispositivos que indicam GPU ou vídeo acessível (DRM, NVIDIA, AMD KFD e a GPU do WSL).
const GPU_DEVICES: &[&str] = &["/dev/dri", "/dev/nvidia0", "/dev/nvidiactl", "/dev/kfd", "/dev/dxg"];

/// Medida genérica em `sh` puro, rodada dentro do serviço `workspace`. Sai sempre com 0.
pub const PROBE_SCRIPT: &str = "g=0; for d in /dev/dri /dev/nvidia0 /dev/nvidiactl /dev/kfd /dev/dxg; do [ -e \"$d\" ] && g=1; done; \
echo \"nants-cap:gpu=$g\"; \
if [ -n \"${DISPLAY:-}${WAYLAND_DISPLAY:-}\" ]; then echo nants-cap:display=1; else echo nants-cap:display=0; fi; \
echo \"nants-cap:cpus=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null)\"; \
if [ -r /sys/fs/cgroup/cpu.max ]; then read -r q p < /sys/fs/cgroup/cpu.max; echo \"nants-cap:cpu_max=$q $p\"; fi; \
if [ -r /proc/meminfo ]; then while read -r k v u; do if [ \"$k\" = MemTotal: ]; then echo \"nants-cap:mem_kb=$v\"; fi; done < /proc/meminfo; fi; \
if [ -r /sys/fs/cgroup/memory.max ]; then read -r m < /sys/fs/cgroup/memory.max; echo \"nants-cap:mem_max=$m\"; fi; \
true";

/// Uma rodada do `test_command`, medida pelo runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteRun {
    /// Duração, em milissegundos.
    pub ms: u64,
    /// Limite da rodada, em milissegundos, quando conhecido.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_ms: Option<u64>,
    pub passed: bool,
    #[serde(default)]
    pub timed_out: bool,
    /// Instante do fim (RFC 3339 UTC).
    pub at: String,
    /// Spec 137 T09: rodada offline (sem internet, pela rede interna do override), a que prova os
    /// critérios `(offline)`. Ausente nas rodadas normais e nos registros antigos.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub offline: bool,
}

impl SuiteRun {
    /// Rodada medida agora: duração `elapsed` contra o limite `limit`.
    pub fn measured(elapsed: Duration, limit: Option<Duration>, passed: bool, timed_out: bool, at: String) -> Self {
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        Self { ms: ms(elapsed), limit_ms: limit.map(ms), passed, timed_out, at, offline: false }
    }

    /// A mesma rodada, marcada como offline.
    pub fn offline(mut self) -> Self {
        self.offline = true;
        self
    }

    /// Duração em segundos, arredondada para cima.
    pub fn secs(&self) -> u64 {
        self.ms.div_ceil(1000)
    }

    /// Limite em segundos, arredondado para cima.
    pub fn limit_secs(&self) -> Option<u64> {
        self.limit_ms.map(|l| l.div_ceil(1000))
    }

    /// Fração do limite usada, em porcentagem; `None` sem limite.
    pub fn percent(&self) -> Option<u64> {
        percent_of(self.ms, self.limit_ms?)
    }
}

/// Porcentagem de `used` em `limit`, na mesma unidade (limite zero: `None`).
pub fn percent_of(used: u64, limit: u64) -> Option<u64> {
    (limit > 0).then(|| used.saturating_mul(100) / limit)
}

/// A duração passa de [`BUDGET_WARN_PERCENT`] do limite?
pub fn over_budget(elapsed: Duration, limit: Option<Duration>) -> bool {
    match limit {
        Some(limit) if !limit.is_zero() => elapsed.as_millis().saturating_mul(100) > limit.as_millis().saturating_mul(u128::from(BUDGET_WARN_PERCENT)),
        _ => false,
    }
}

/// A repetição de uma rodada que levou `elapsed` cabe no que sobra do limite? Sem limite conhecido
/// não há como saber, então não cabe.
pub fn rerun_fits(elapsed: Duration, limit: Option<Duration>) -> bool {
    match limit {
        Some(limit) => elapsed <= limit.saturating_sub(elapsed),
        None => false,
    }
}

/// Capacidades medidas do ambiente de testes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// `docker` ou `host`; vazio = nunca medido.
    #[serde(default)]
    pub environment: String,
    /// Chave do ambiente medido (hash dos arquivos de `.nants/` no Docker, `host` no host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u64>,
    /// Instante da medida (RFC 3339 UTC).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_at: Option<String>,
    /// Últimas rodadas do `test_command`, da mais antiga para a mais recente.
    #[serde(default)]
    pub runs: Vec<SuiteRun>,
}

/// O que a medida achou (sem durações).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probe {
    pub gpu: Option<bool>,
    pub display: Option<bool>,
    pub cpus: Option<u32>,
    pub memory_mb: Option<u64>,
}

/// Lê as linhas `nants-cap:chave=valor` da saída da medida. Linhas fora do padrão são ignoradas;
/// chave sem valor válido fica desconhecida.
pub fn parse_probe(output: &str) -> Probe {
    let mut probe = Probe::default();
    let (mut cpu_max, mut mem_kb, mut mem_max) = (None, None, None);
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix(PROBE_MARK) else { continue };
        let Some((key, value)) = rest.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "gpu" => probe.gpu = flag(value),
            "display" => probe.display = flag(value),
            "cpus" => probe.cpus = value.parse::<u32>().ok().filter(|n| *n > 0),
            "cpu_max" => cpu_max = quota_cpus(value),
            "mem_kb" => mem_kb = value.parse::<u64>().ok().filter(|n| *n > 0),
            "mem_max" => mem_max = value.parse::<u64>().ok().filter(|n| *n > 0),
            _ => {}
        }
    }
    // A cota do cgroup (quando há) limita os núcleos e a memória vistos pelo sistema.
    probe.cpus = match (probe.cpus, cpu_max) {
        (Some(n), Some(q)) => Some(n.min(q)),
        (n, q) => n.or(q),
    };
    let total = mem_kb.map(|kb| kb / 1024);
    let limit = mem_max.map(|b| b / (1024 * 1024));
    probe.memory_mb = match (total, limit) {
        (Some(t), Some(l)) => Some(t.min(l)),
        (t, l) => t.or(l),
    }
    .filter(|mb| *mb > 0);
    probe
}

fn flag(value: &str) -> Option<bool> {
    match value {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// `cpu.max` do cgroup v2 (`<cota> <período>`; `max` = sem cota) em núcleos, arredondado para cima.
fn quota_cpus(value: &str) -> Option<u32> {
    let mut parts = value.split_whitespace();
    let quota = parts.next()?.parse::<u64>().ok()?;
    let period = parts.next()?.parse::<u64>().ok().filter(|p| *p > 0)?;
    u32::try_from(quota.div_ceil(period)).ok().filter(|n| *n > 0)
}

/// Medida no host, das mesmas fontes que o [`PROBE_SCRIPT`] usa no contêiner.
pub fn probe_host() -> Probe {
    let mut text = String::new();
    let gpu = GPU_DEVICES.iter().any(|d| Path::new(d).exists());
    text.push_str(&format!("{PROBE_MARK}gpu={}\n", u8::from(gpu)));
    let display = ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
    text.push_str(&format!("{PROBE_MARK}display={}\n", u8::from(display)));
    if let Ok(n) = std::thread::available_parallelism() {
        text.push_str(&format!("{PROBE_MARK}cpus={n}\n"));
    }
    if let Ok(meminfo) = fs::read_to_string("/proc/meminfo") {
        if let Some(kb) = meminfo.lines().find_map(|l| l.strip_prefix("MemTotal:")).and_then(|r| r.split_whitespace().next()) {
            text.push_str(&format!("{PROBE_MARK}mem_kb={kb}\n"));
        }
    }
    if let Ok(max) = fs::read_to_string("/sys/fs/cgroup/memory.max") {
        text.push_str(&format!("{PROBE_MARK}mem_max={}\n", max.trim()));
    }
    parse_probe(&text)
}

impl Capabilities {
    /// Grava a medida `probe` do ambiente `environment` (chave `env_key`), mantendo as rodadas.
    pub fn apply_probe(&mut self, environment: &str, env_key: &str, probe: Probe, at: String) {
        if !self.environment.is_empty() && self.environment != environment {
            // Outro ambiente: as durações medidas no anterior não valem para este.
            self.runs.clear();
        }
        self.environment = environment.to_string();
        self.env_key = Some(env_key.to_string());
        self.gpu = probe.gpu;
        self.display = probe.display;
        self.cpus = probe.cpus;
        self.memory_mb = probe.memory_mb;
        self.measured_at = Some(at);
    }

    /// Acrescenta uma rodada, guardando só as [`MAX_RUNS`] mais recentes.
    pub fn push_run(&mut self, run: SuiteRun) {
        self.runs.push(run);
        let excess = self.runs.len().saturating_sub(MAX_RUNS);
        self.runs.drain(..excess);
    }

    /// Nada medido nem registrado.
    pub fn is_empty(&self) -> bool {
        self.environment.is_empty() && self.runs.is_empty()
    }

    /// Última rodada acima de [`BUDGET_WARN_PERCENT`] do limite, quando houver.
    pub fn budget_warning(&self) -> Option<&SuiteRun> {
        self.runs.last().filter(|r| r.percent().is_some_and(|p| p > BUDGET_WARN_PERCENT))
    }
}

/// Capacidades para quem lê antes de qualquer rodada (a revisão do plano): as gravadas; sem nada
/// gravado e com o ambiente no host, a medida do host feita agora (barata, sem gravar). No Docker
/// sem medida, vazio: a medida acontece dentro do contêiner, na primeira rodada.
pub fn current(root: &Path, host: bool, at: impl FnOnce() -> String) -> Capabilities {
    let caps = read(root);
    if !caps.is_empty() || !host {
        return caps;
    }
    let mut fresh = Capabilities::default();
    fresh.apply_probe("host", "host", probe_host(), at());
    fresh
}

/// Lê o estado do projeto. Ausente, ilegível, symlink ou inválido: vazio (melhor esforço).
pub fn read(root: &Path) -> Capabilities {
    let path = root.join(CAPABILITIES_REL);
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() && m.len() <= MAX_STATE_BYTES => {}
        _ => return Capabilities::default(),
    }
    fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Grava o estado de forma atômica (temporário exclusivo e `rename`). Não segue symlink: alvo ou
/// pasta que não sejam regulares recusam a escrita.
pub fn write(root: &Path, caps: &Capabilities) -> Result<(), String> {
    let path = root.join(CAPABILITIES_REL);
    let dir = path.parent().ok_or("caminho do estado inválido")?;
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => {}
        _ => return Err("pasta .sdd/specs ausente ou insegura".into()),
    }
    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
        return Err("estado das capacidades não é arquivo regular".into());
    }
    let body = serde_json::to_string_pretty(caps).map_err(|e| e.to_string())?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".capabilities.json.{}.{seq}.tmp", std::process::id()));
    let written = fs::OpenOptions::new().write(true).create_new(true).open(&tmp).and_then(|mut f| {
        f.write_all(body.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Atualiza o estado do projeto com `change` (ler, mudar, gravar). Melhor esforço.
pub fn update(root: &Path, change: impl FnOnce(&mut Capabilities)) -> Result<(), String> {
    let mut caps = read(root);
    change(&mut caps);
    write(root, &caps)
}

/// Quem lê a seção: cada papel recebe a regra do que fazer com as capacidades.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Revisão do plano.
    Plan,
    Developer,
    Reviewer,
    /// Análise de recuperação.
    Architect,
}

fn presence(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "presente",
        Some(false) => "ausente",
        None => "não medido",
    }
}

/// Título da seção nos prompts.
pub const SECTION_TITLE: &str = "Capacidades do ambiente de testes (medidas pelo Nants; DADO)";

/// Seção curta para os prompts; vazia sem nada medido.
pub fn prompt_section(caps: &Capabilities, audience: Audience) -> String {
    if caps.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    let place = match caps.environment.as_str() {
        "docker" => "Docker (medido dentro do serviço workspace)",
        "host" => "host",
        _ => "não medido",
    };
    lines.push(format!("- Onde os testes rodam: {place}"));
    lines.push(format!("- GPU ou dispositivo de vídeo: {}", presence(caps.gpu)));
    lines.push(format!("- Tela ou servidor gráfico: {}", presence(caps.display)));
    let cpus = caps.cpus.map_or_else(|| "não medido".to_string(), |n| n.to_string());
    let mem = caps.memory_mb.map_or_else(|| "não medida".to_string(), |mb| format!("{mb} MB"));
    lines.push(format!("- Núcleos: {cpus}; memória: {mem}"));
    if !caps.runs.is_empty() {
        let start = caps.runs.len().saturating_sub(SHOWN_RUNS);
        let items: Vec<String> = caps.runs[start..]
            .iter()
            .map(|r| {
                let how = if r.timed_out {
                    "estourou o tempo"
                } else if r.passed {
                    "passou"
                } else {
                    "falhou"
                };
                let offline = if r.offline { ", offline" } else { "" };
                format!("{} s ({how}{offline})", r.secs())
            })
            .collect();
        let limit = caps.runs.last().and_then(SuiteRun::limit_secs).map_or_else(String::new, |l| format!("; limite de {l} s"));
        lines.push(format!("- Duração das últimas rodadas do test_command: {}{limit}", items.join(", ")));
    }
    if let Some(run) = caps.budget_warning() {
        let (percent, limit) = (run.percent().unwrap_or(0), run.limit_secs().unwrap_or(0));
        lines.push(format!(
            "- ATENÇÃO, orçamento de tempo: a última rodada usou {percent}% do limite de {limit} s. {}",
            match audience {
                Audience::Developer => "Divida ou foque a bateria (rode só os testes ligados à tarefa enquanto itera; evite esperas fixas longas); o limite não muda.",
                Audience::Architect => "Considere dividir a tarefa ou focar a bateria (REVISE_CONFIG ou SPLIT); o limite não muda.",
                Audience::Plan | Audience::Reviewer => "A bateria está perto do limite (sinal, não corte).",
            }
        ));
    }
    let rule = role_rule(caps, audience);
    format!("## {SECTION_TITLE}\n{}\n{rule}\n", lines.join("\n"))
}

/// Capacidades medidas como ausentes (`Some(false)`), pelo nome usado no bloco. Presente ou não
/// medido não entra.
pub fn absent(caps: &Capabilities) -> Vec<&'static str> {
    let mut out = Vec::new();
    if caps.gpu == Some(false) {
        out.push("GPU ou dispositivo de vídeo");
    }
    if caps.display == Some(false) {
        out.push("tela ou servidor gráfico");
    }
    out
}

/// Regra do papel, presa ao que o bloco mede como ausente: só capacidade `Some(false)` justifica
/// verificação manual no lugar do teste automático; presente ou não medido não dispensa nada.
fn role_rule(caps: &Capabilities, audience: Audience) -> String {
    let missing = absent(caps);
    let list = missing.join(" e ");
    const NOTHING: &str = "Presente ou não medido não dispensa nada.";
    match (audience, missing.is_empty()) {
        (Audience::Plan, true) => format!("Nenhuma capacidade medida como ausente: o ambiente não justifica apontar critério como untestable. {NOTHING}"),
        (Audience::Plan, false) => format!(
            "Ausente neste ambiente: {list}. Critério que depende disso não pode ser provado aqui: aponte-o como untestable e sugira verificação manual documentada. {NOTHING}"
        ),
        (Audience::Developer, true) => format!(
            "Escreva testes adequados a este ambiente: espere por uma condição (elemento, resposta, evento) em vez de tempo fixo. Nenhuma capacidade medida como ausente: todo critério continua com teste automático. {NOTHING}"
        ),
        (Audience::Developer, false) => {
            let gpu = if caps.gpu == Some(false) {
                " Sem GPU, nada de igualdade exata de captura de tela ou de pixel (use tolerância ou confira propriedades)."
            } else {
                ""
            };
            format!(
                "Escreva testes adequados a este ambiente: espere por uma condição (elemento, resposta, evento) em vez de tempo fixo.{gpu} Ausente neste ambiente: {list}. Só o que depende disso vira verificação manual documentada (passos e resultado esperado), não teste que finge provar; o resto continua com teste automático. {NOTHING}"
            )
        }
        (Audience::Reviewer, true) => format!(
            "Nenhuma capacidade medida como ausente: nenhum critério é dispensado de teste automático por causa do ambiente. {NOTHING}"
        ),
        (Audience::Reviewer, false) => format!(
            "Ausente neste ambiente: {list}. Só critério que depende disso dispensa o teste automático, e mesmo assim cobre a verificação manual documentada (passos e resultado esperado). Os demais critérios seguem cobrados normalmente. {NOTHING}"
        ),
        (Audience::Architect, true) => format!("Nenhuma capacidade medida como ausente: o ambiente não justifica ADJUST. {NOTHING}"),
        (Audience::Architect, false) => format!(
            "Ausente neste ambiente: {list}. Critério que depende disso é candidato a ADJUST (verificação manual documentada), não a outro RETRY. {NOTHING}"
        ),
    }
}

/// Linhas da saída de uma rodada que falhou que parecem nomear testes que falharam, em qualquer
/// stack (`FAIL`, `FAILED`, `failed`, `not ok`, `✕`, `✘`, `×`). Até `max` linhas distintas, cada uma
/// com até 200 caracteres. Sem nenhuma, vazio.
pub fn failing_lines(output: &str, max: usize) -> Vec<String> {
    const MARKS: &[&str] = &["FAIL", "failed", "Failed", "not ok", "✕", "✘", "×", "panicked"];
    let mut out: Vec<String> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || !MARKS.iter().any(|m| line.contains(m)) {
            continue;
        }
        let short: String = line.chars().take(200).collect();
        if !out.contains(&short) {
            out.push(short);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Spec 133 T05: as linhas de falha ([`failing_lines`]) com as durações trocadas por `<t>`, para
/// comparar duas rodadas da mesma falha (a duração muda a cada rodada, em qualquer stack).
pub fn failure_signature(output: &str, max: usize) -> Vec<String> {
    failing_lines(output, max).iter().map(|l| without_durations(l)).collect()
}

/// Spec 139 T01: a parte das linhas da assinatura da falha que o gate compara entre dois ciclos.
/// São as linhas de [`failure_signature`] (os testes que falharam) seguidas das últimas `max` linhas
/// não vazias da saída (a mensagem, que muda quando o Desenvolvedor avança dentro do mesmo teste).
/// Em todas, as durações, os horários e datas, os números de porta e o trecho variável dos caminhos
/// temporários viram marcas fixas: duas rodadas da mesma falha dão as mesmas linhas, em qualquer
/// stack. O ruído que sobrar só faz a falha parecer outra, o lado seguro (o gate não para cedo).
pub fn stable_failure_lines(output: &str, max: usize) -> Vec<String> {
    let mut lines = failure_signature(output, max);
    let tail: Vec<&str> = output.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let from = tail.len().saturating_sub(max);
    lines.extend(tail[from..].iter().map(|l| without_durations(&l.chars().take(200).collect::<String>())));
    lines.iter().map(|l| without_volatile(l)).collect()
}

/// O texto termina na palavra `port` seguida de espaço, `=` ou `: ` (`import ` e `report ` não).
fn ends_with_port_word(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["port ", "port=", "port: "].iter().any(|suffix| {
        lower.strip_suffix(suffix).is_some_and(|before| !before.chars().next_back().is_some_and(char::is_alphanumeric))
    })
}

/// Começos de caminho temporário (Unix, macOS e Windows) e a pasta temporária deste processo.
fn temp_prefixes() -> Vec<String> {
    let mut out: Vec<String> =
        ["/tmp/", "/var/tmp/", "/var/folders/", "/private/var/folders/", "/private/tmp/", "\\Temp\\"].iter().map(|p| p.to_string()).collect();
    let own = std::env::temp_dir().to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if own.len() > 1 {
        out.push(format!("{own}/"));
    }
    out
}

/// Fim de um trecho de caminho: espaço, aspas, parênteses, colchetes, vírgula ou dois-pontos.
fn path_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '<' | '>' | ',' | ':' | ';')
}

/// Troca o caminho até a pasta variável de um temporário (`/tmp/abc123/src/x.js` vira
/// `<tmp>/src/x.js`; no Windows, `Users\x\AppData\Local\Temp\abc` vira `<tmp>`). O resto do
/// caminho fica, para a falha continuar distinguível.
fn without_temp_paths(line: &str) -> String {
    let mut out = line.to_string();
    for prefix in temp_prefixes() {
        let mut from = 0;
        while let Some(rel) = out[from..].find(&prefix) {
            let at = from + rel;
            let start = out[..at].rfind(path_delimiter).map_or(0, |d| d + out[d..].chars().next().map_or(1, char::len_utf8));
            let after = at + prefix.len();
            // O começo Unix (`/tmp/`) só vale no início do caminho: `/home/x/tmp/` não é temporário.
            if prefix.starts_with('/') && start != at {
                from = after;
                continue;
            }
            let end = out[after..].find(|c: char| path_delimiter(c) || c == '/' || c == '\\').map_or(out.len(), |e| after + e);
            out.replace_range(start..end, "<tmp>");
            from = start + "<tmp>".len();
        }
    }
    out
}

/// O texto antes da posição termina num nome de máquina que leva porta: `localhost`, um IPv4 ou um
/// endereço entre colchetes (`[::1]`).
fn ends_with_host(before: &str) -> bool {
    if before.ends_with("localhost") || before.ends_with(']') {
        return true;
    }
    let token = before.rsplit(|c: char| !(c.is_ascii_digit() || c == '.')).next().unwrap_or("");
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.parse::<u8>().is_ok())
}

/// Fim da sequência de dígitos que começa em `i`.
fn digits_end(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    j
}

/// `hh:mm:ss` (com fração opcional) a partir de `i`: o fim, se houver.
fn clock_end(chars: &[char], i: usize) -> Option<usize> {
    let h = digits_end(chars, i);
    if !(1..=2).contains(&(h - i)) || chars.get(h) != Some(&':') {
        return None;
    }
    let m = digits_end(chars, h + 1);
    if m - h - 1 != 2 || chars.get(m) != Some(&':') {
        return None;
    }
    let sec = digits_end(chars, m + 1);
    if sec - m - 1 != 2 {
        return None;
    }
    let mut end = sec;
    if matches!(chars.get(end), Some('.' | ',')) && chars.get(end + 1).is_some_and(char::is_ascii_digit) {
        end = digits_end(chars, end + 1);
    }
    Some(end)
}

/// `aaaa-mm-dd` a partir de `i`: o fim, se houver.
fn date_end(chars: &[char], i: usize) -> Option<usize> {
    let y = digits_end(chars, i);
    if y - i != 4 || chars.get(y) != Some(&'-') {
        return None;
    }
    let m = digits_end(chars, y + 1);
    if m - y - 1 != 2 || chars.get(m) != Some(&'-') {
        return None;
    }
    let d = digits_end(chars, m + 1);
    (d - m - 1 == 2).then_some(d)
}

/// Spec 139 T01: troca o que varia entre duas rodadas da mesma falha por marcas fixas: o trecho
/// variável dos caminhos temporários (`<tmp>`), datas (`<date>`), horários `hh:mm:ss` (`<clock>`)
/// e números de porta depois de `localhost`, de um IPv4, de `[...]` ou da palavra `port` (`<port>`).
/// Número de linha e coluna (`x.js:10:5`) fica: ele distingue uma falha da outra.
fn without_volatile(line: &str) -> String {
    let chars: Vec<char> = without_temp_paths(line).chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev = i.checked_sub(1).map(|p| chars[p]);
        if c == ':' && ends_with_host(&out) {
            let j = digits_end(&chars, i + 1);
            if (1..=5).contains(&(j - i - 1)) {
                out.push_str(":<port>");
                i = j;
                continue;
            }
        }
        if c.is_ascii_digit() && !prev.is_some_and(|p| p.is_ascii_digit() || p == '.' || p == ':') {
            if let Some(end) = date_end(&chars, i) {
                out.push_str("<date>");
                i = end;
                continue;
            }
            if let Some(end) = clock_end(&chars, i) {
                out.push_str("<clock>");
                i = end;
                continue;
            }
            if ends_with_port_word(&out) {
                out.push_str("<port>");
                i = digits_end(&chars, i);
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Troca cada número seguido de unidade de tempo (`1.23s`, `0,5 s`, `120ms`, `3 min`, `2µs`...) por
/// `<t>`. O resto da linha fica igual.
fn without_durations(line: &str) -> String {
    const UNITS: &[&str] = &[
        "ns", "us", "µs", "μs", "ms", "s", "sec", "secs", "second", "seconds", "m", "min", "mins", "minute", "minutes", "h",
        "hr", "hrs", "hour", "hours",
    ];
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let starts_number = c.is_ascii_digit() && (i == 0 || !chars[i - 1].is_alphanumeric());
        if !starts_number {
            out.push(c);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < chars.len() && (chars[j].is_ascii_digit() || ((chars[j] == '.' || chars[j] == ',') && chars.get(j + 1).is_some_and(char::is_ascii_digit))) {
            j += 1;
        }
        let mut k = j;
        while k < chars.len() && chars[k] == ' ' {
            k += 1;
        }
        let mut end = k;
        while end < chars.len() && chars[end].is_alphabetic() {
            end += 1;
        }
        let unit: String = chars[k..end].iter().collect::<String>().to_lowercase();
        if end > k && UNITS.contains(&unit.as_str()) {
            out.push_str("<t>");
            i = end;
        } else {
            out.extend(&chars[i..j]);
            i = j;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(secs: u64, limit: Option<u64>, passed: bool) -> SuiteRun {
        SuiteRun::measured(Duration::from_secs(secs), limit.map(Duration::from_secs), passed, false, "2026-10-24T00:00:00Z".into())
    }

    #[test]
    fn probe_output_is_parsed_with_cgroup_limits() {
        let out = "motd do contêiner\nnants-cap:gpu=0\nnants-cap:display=1\nnants-cap:cpus=16\nnants-cap:cpu_max=400000 100000\nnants-cap:mem_kb=32768000\nnants-cap:mem_max=4294967296\n";
        let p = parse_probe(out);
        assert_eq!(p, Probe { gpu: Some(false), display: Some(true), cpus: Some(4), memory_mb: Some(4096) });
        // Sem cota (`max`) valem os números do sistema; linhas ruins ficam desconhecidas.
        let p = parse_probe("nants-cap:gpu=1\nnants-cap:cpus=\nnants-cap:cpu_max=max 100000\nnants-cap:mem_kb=2048000\nnants-cap:mem_max=max\n");
        assert_eq!(p, Probe { gpu: Some(true), display: None, cpus: None, memory_mb: Some(2000) });
        assert_eq!(parse_probe(""), Probe::default());
    }

    #[test]
    fn host_probe_measures_cpus_without_failing() {
        let p = probe_host();
        assert!(p.cpus.is_some_and(|n| n > 0), "{p:?}");
        assert!(p.gpu.is_some() && p.display.is_some(), "{p:?}");
    }

    #[test]
    fn state_round_trips_outside_git_and_keeps_last_runs() {
        let root = std::env::temp_dir().join(format!("nants-caps-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        // Sem `.sdd/specs`, a escrita recusa e a leitura devolve vazio.
        assert!(write(&root, &Capabilities::default()).is_err());
        assert!(read(&root).is_empty());
        fs::create_dir_all(root.join(".sdd/specs")).expect("mkdir");
        for i in 0..7 {
            update(&root, |c| c.push_run(run(i, Some(600), true))).expect("update");
        }
        update(&root, |c| c.apply_probe("docker", "abc", Probe { gpu: Some(false), ..Probe::default() }, "t".into())).expect("probe");
        let caps = read(&root);
        assert_eq!(caps.runs.len(), MAX_RUNS);
        assert_eq!(caps.runs.first().map(SuiteRun::secs), Some(2));
        assert_eq!((caps.environment.as_str(), caps.gpu), ("docker", Some(false)));
        // Outro ambiente zera as durações do anterior.
        update(&root, |c| c.apply_probe("host", "host", Probe::default(), "t".into())).expect("host");
        assert!(read(&root).runs.is_empty());
        let leftovers: Vec<_> = fs::read_dir(root.join(".sdd/specs")).expect("ls").flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn budget_and_rerun_rules() {
        let limit = Some(Duration::from_secs(600));
        assert!(!over_budget(Duration::from_secs(420), limit));
        assert!(over_budget(Duration::from_secs(421), limit));
        assert!(!over_budget(Duration::from_secs(10_000), None));
        assert!(rerun_fits(Duration::from_secs(300), limit));
        assert!(!rerun_fits(Duration::from_secs(301), limit));
        assert!(!rerun_fits(Duration::ZERO, None), "sem limite conhecido não dá para saber se cabe");
        let r = SuiteRun::measured(Duration::from_millis(200), Some(Duration::from_millis(250)), false, false, String::new());
        assert_eq!((r.secs(), r.limit_secs(), r.percent()), (1, Some(1), Some(80)));
    }

    /// Spec 137 T09: a rodada offline entra no registro de rodadas com a marca, e o registro antigo
    /// (sem o campo) segue legível, como rodada normal.
    #[test]
    fn offline_runs_are_marked_in_the_record() {
        let mut caps = Capabilities::default();
        caps.push_run(run(30, Some(600), true));
        caps.push_run(run(40, Some(600), true).offline());
        let json = serde_json::to_string(&caps).expect("json");
        assert_eq!(json.matches("\"offline\":true").count(), 1, "{json}");
        assert!(!json.contains("\"offline\":false"), "a rodada normal não ganha o campo: {json}");
        let back: Capabilities = serde_json::from_str(&json).expect("ler");
        assert_eq!(back.runs.iter().map(|r| r.offline).collect::<Vec<_>>(), vec![false, true]);
        let old: SuiteRun = serde_json::from_str("{\"ms\":1000,\"passed\":true,\"at\":\"t\"}").expect("antigo");
        assert!(!old.offline);
        let text = prompt_section(&caps, Audience::Reviewer);
        assert!(text.contains("30 s (passou), 40 s (passou, offline)"), "{text}");
    }

    #[test]
    fn prompt_section_carries_capabilities_budget_and_role_rule() {
        assert!(prompt_section(&Capabilities::default(), Audience::Developer).is_empty());
        let mut caps = Capabilities::default();
        caps.apply_probe("docker", "h", Probe { gpu: Some(false), display: Some(false), cpus: Some(4), memory_mb: Some(8000) }, "t".into());
        caps.push_run(run(300, Some(600), true));
        caps.push_run(run(540, Some(600), false));
        let dev = prompt_section(&caps, Audience::Developer);
        assert!(dev.contains("GPU ou dispositivo de vídeo: ausente"), "{dev}");
        assert!(dev.contains("Tela ou servidor gráfico: ausente"), "{dev}");
        assert!(dev.contains("Núcleos: 4; memória: 8000 MB"), "{dev}");
        assert!(dev.contains("300 s (passou), 540 s (falhou); limite de 600 s"), "{dev}");
        assert!(dev.contains("usou 90% do limite de 600 s") && dev.contains("Divida ou foque a bateria"), "{dev}");
        assert!(dev.contains("espere por uma condição") && dev.contains("igualdade exata de captura"), "{dev}");
        let rev = prompt_section(&caps, Audience::Reviewer);
        assert!(rev.contains("Ausente neste ambiente: GPU ou dispositivo de vídeo e tela ou servidor gráfico."), "{rev}");
        assert!(rev.contains("Só critério que depende disso dispensa o teste automático") && rev.contains("cobre a verificação manual documentada (passos e resultado esperado)"), "{rev}");
        let plan = prompt_section(&caps, Audience::Plan);
        assert!(plan.contains("aponte-o como untestable"), "{plan}");
        let arch = prompt_section(&caps, Audience::Architect);
        assert!(arch.contains("candidato a ADJUST") && arch.contains("REVISE_CONFIG ou SPLIT"), "{arch}");
        // Tudo presente ou não medido (ex.: sonda do Docker falhou, só rodadas gravadas): nenhuma dispensa.
        let mut full = caps.clone();
        full.apply_probe("docker", "h", Probe { gpu: Some(true), display: Some(true), cpus: Some(4), memory_mb: None }, "t".into());
        let mut unmeasured = Capabilities::default();
        unmeasured.push_run(run(10, Some(600), true));
        for c in [&full, &unmeasured] {
            let rev = prompt_section(c, Audience::Reviewer);
            assert!(rev.contains("nenhum critério é dispensado de teste automático"), "{rev}");
            assert!(!rev.contains("dispensa o teste automático,") && !rev.contains("Ausente neste ambiente"), "{rev}");
            let dev = prompt_section(c, Audience::Developer);
            assert!(!dev.contains("vira verificação manual") && !dev.contains("Sem GPU"), "{dev}");
            assert!(prompt_section(c, Audience::Plan).contains("não justifica apontar critério como untestable"));
            assert!(prompt_section(c, Audience::Architect).contains("não justifica ADJUST"));
        }
        // Só a tela ausente: a regra de captura sem GPU não aparece.
        let mut no_display = full.clone();
        no_display.display = Some(false);
        let dev = prompt_section(&no_display, Audience::Developer);
        assert!(dev.contains("Ausente neste ambiente: tela ou servidor gráfico.") && !dev.contains("Sem GPU"), "{dev}");
        // Abaixo de 70% do limite, sem aviso.
        caps.push_run(run(100, Some(600), true));
        assert!(!prompt_section(&caps, Audience::Developer).contains("ATENÇÃO"));
    }

    #[test]
    fn failing_lines_are_generic_and_bounded() {
        let out = "running 3 tests\ntest a ... ok\ntest b ... FAILED\n  ✘ [chromium] › e2e/two.spec.ts:10 two clients\n×  render matches\ntest b ... FAILED\n";
        let lines = failing_lines(out, 10);
        assert_eq!(lines, vec!["test b ... FAILED", "✘ [chromium] › e2e/two.spec.ts:10 two clients", "×  render matches"]);
        assert_eq!(failing_lines(out, 1).len(), 1);
        assert!(failing_lines("tudo certo", 5).is_empty());
    }

    /// Spec 133 T05: a mesma falha com outra duração tem a mesma assinatura (cargo e pytest); outro
    /// teste falhando, não.
    #[test]
    fn failure_signature_ignores_durations_only() {
        let cargo = |secs: &str, name: &str| {
            format!("test {name} ... FAILED\nfailures:\n    {name}\ntest result: FAILED. 3 passed; 1 failed; 0 ignored; finished in {secs}\n")
        };
        assert_eq!(failure_signature(&cargo("1.23s", "a::b"), 10), failure_signature(&cargo("0.98s", "a::b"), 10));
        assert_ne!(failure_signature(&cargo("1.23s", "a::b"), 10), failure_signature(&cargo("1.23s", "a::c"), 10));
        let pytest = |secs: &str, name: &str| {
            format!("FAILED tests/test_api.py::{name} - assert 1 == 2\n=== 1 failed, 3 passed in {secs} ===\n")
        };
        assert_eq!(failure_signature(&pytest("0.52s", "test_x"), 10), failure_signature(&pytest("12.07s", "test_x"), 10));
        assert_ne!(failure_signature(&pytest("0.52s", "test_x"), 10), failure_signature(&pytest("0.52s", "test_y"), 10));
        // Outras durações e números que não são tempo.
        assert_eq!(without_durations("✘ two.spec.ts:10 (1.5 s) [120ms] 3 min v2"), "✘ two.spec.ts:10 (<t>) [<t>] <t> v2");
        assert_eq!(without_durations("1 failed, 3 passed"), "1 failed, 3 passed");
    }

    /// Spec 139 T01: a assinatura do gate ignora tempos, horários, portas e o trecho variável dos
    /// caminhos temporários; sem linha de teste que falhou, compara o fim da saída.
    #[test]
    fn stable_failure_lines_ignore_times_ports_and_temp_paths() {
        let run = |dir: &str, port: &str, clock: &str, secs: &str| {
            format!(
                "[{clock}] server on http://localhost:{port}\n✘ e2e/cart.spec.ts:12:5 adds item ({secs})\n    at {dir}/src/cart.ts:40:7\nError: connect ECONNREFUSED 127.0.0.1:{port}\n1 failed\n"
            )
        };
        let a = stable_failure_lines(&run("/tmp/nants-abc123", "41234", "10:46:12", "1.2s"), 8);
        let b = stable_failure_lines(&run("/tmp/nants-zz9", "39001", "10:48:59.120", "3.4s"), 8);
        assert_eq!(a, b);
        assert!(a.iter().any(|l| l.contains("e2e/cart.spec.ts:12:5")), "linha e coluna ficam: {a:?}");
        // A falha que muda continua diferente.
        let other = stable_failure_lines(&run("/tmp/nants-abc123", "41234", "10:46:12", "1.2s").replace("cart.spec.ts:12:5", "login.spec.ts:3:1"), 8);
        assert_ne!(a, other);
        // Sem linha de teste que falhou: o fim da saída, com as mesmas trocas.
        let none = |at: &str, port: &str| format!("Running on port {port}\n{at} Error: No tests found\n");
        let x = stable_failure_lines(&none("2026-10-31T10:46:01", "5173"), 8);
        assert_eq!(x, stable_failure_lines(&none("2026-11-01T09:00:59", "6006"), 8));
        assert!(!x.is_empty() && x.iter().any(|l| l.contains("No tests found")), "{x:?}");
        assert_ne!(x, stable_failure_lines("Error: Cannot find module 'x'\n", 8));
        // Caminho de projeto com `tmp` no meio não é temporário.
        assert_eq!(without_volatile("FAIL /home/u/tmp/app.test.js"), "FAIL /home/u/tmp/app.test.js");
        assert_eq!(without_volatile("FAIL /tmp/run-1/app.test.js"), "FAIL <tmp>/app.test.js");
        // Porta só depois da palavra `port`, não de `import` ou `report`.
        assert_eq!(without_volatile("port 3000, Port=8080, port: 5173"), "port <port>, Port=<port>, port: <port>");
        assert_eq!(without_volatile("import 3 and report 4"), "import 3 and report 4");
    }

    /// Spec 139 T01: o mesmo teste que falha com outra mensagem (o Desenvolvedor corrigiu a primeira
    /// asserção e caiu na segunda) é outra falha, em qualquer formato de linha marcada.
    #[test]
    fn stable_failure_lines_tell_apart_messages_of_the_same_failing_test() {
        for (head, tail) in [("FAIL tests/cart.test.js", "Tests: 1 failed, 3 passed"), ("✕ adds item (12 ms)", "1 failed"), ("not ok 2 - adds item", "# fail 1")] {
            let run = |message: &str| format!("{head}\n  {message}\n{tail}\n");
            let first = stable_failure_lines(&run("expected 2 to equal 3"), 10);
            let second = stable_failure_lines(&run("expected 'Cart' to equal 'Carrinho'"), 10);
            assert_ne!(first, second, "{head}");
            assert_eq!(first, stable_failure_lines(&run("expected 2 to equal 3"), 10), "{head}");
        }
        // Mais de dez testes falhando: o corte das linhas marcadas não esconde a mudança no fim.
        let many = |last: &str| (1..=12).map(|n| format!("FAIL t{n}.test.js\n")).collect::<String>() + last;
        assert_ne!(stable_failure_lines(&many("FAIL t13.test.js\n"), 10), stable_failure_lines(&many("FAIL t14.test.js\n"), 10));
    }
}
