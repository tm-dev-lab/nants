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
}

impl SuiteRun {
    /// Rodada medida agora: duração `elapsed` contra o limite `limit`.
    pub fn measured(elapsed: Duration, limit: Option<Duration>, passed: bool, timed_out: bool, at: String) -> Self {
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        Self { ms: ms(elapsed), limit_ms: limit.map(ms), passed, timed_out, at }
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
                format!("{} s ({how})", r.secs())
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
}
