//! Spec 134 T08: limites de memória e de processos de todo container que o Nants sobe para o
//! projeto (comandos do Desenvolvedor, suíte, checks, app, console e testes de navegador).
//!
//! O incidente de 2026-10-29: um teste gerado pelo modelo chamava a si mesmo (recursão de
//! processos), o container `workspace` não tinha limite de pids nem de memória e levou a memória
//! e a swap do host ao fim, derrubando a máquina. Daqui em diante:
//! - o teto vem da máquina ([`ContainerLimits::host`]), nunca de um número fixo para todos;
//! - no `compose.yaml` do projeto, um valor menor vale; um maior, ou nenhum, vira o teto
//!   ([`ContainerLimits::effective`]); o projeto não desliga a proteção;
//! - o resultado de um comando que morreu por um dos limites diz isso ([`detect_hit`]).

use std::sync::OnceLock;

use serde_yaml::{Mapping, Value};

const MIB: u64 = 1024 * 1024;

/// Fração da RAM total que um container do projeto pode usar: metade. A outra metade fica para o
/// que o host precisa ao mesmo tempo (kernel, daemon do Docker, o servidor do Nants, o navegador
/// dos testes e a área de trabalho de quem usa). Builds comuns (cargo, webpack, gradle) cabem em
/// 2 a 4 GB, o que uma máquina de 8 GB já entrega.
const MEMORY_SHARE_DIVISOR: u64 = 2;
/// Piso do teto de memória: abaixo disso nem um `npm ci` termina. Só pesa em máquina muito pequena.
const MIN_MEMORY: u64 = 512 * MIB;
/// Teto de memória quando a RAM total não pode ser lida (`/proc/meminfo` ausente): metade de uma
/// máquina de desenvolvimento de 8 GB, o caso mais comum, sem chegar perto de derrubá-la.
const FALLBACK_MEMORY: u64 = 4 * 1024 * MIB;
/// Processos e threads por MiB do teto de memória. Cada tarefa custa ao menos cerca de 1 MiB
/// (pilha do kernel, tabelas de página, páginas privadas); permitir mais tarefas do que a memória
/// comporta só atrasa a falta de memória e deixa o host lento antes dela.
const PIDS_PER_MIB: u64 = 1;
/// Piso do limite de processos: um build paralelo normal (cargo com um rustc por núcleo, Jest
/// com um worker por núcleo, cada um com suas threads, o Chromium dos testes) fica abaixo de mil.
const MIN_PIDS: u64 = 1024;
/// Teto do limite de processos: o bastante para qualquer build que conhecemos e pequeno o bastante
/// para uma recursão de processos parar em segundos, não em minutos.
const MAX_PIDS: u64 = 4096;

/// Limites de um container: memória (bytes, sem swap além dela) e processos/threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerLimits {
    pub memory: u64,
    pub pids: u64,
}

impl ContainerLimits {
    /// Teto derivado da RAM total da máquina (`MemTotal`, em bytes; `None` se ilegível).
    pub fn from_mem_total(total: Option<u64>) -> Self {
        let memory = match total {
            Some(t) if t > 0 => (t / MEMORY_SHARE_DIVISOR / MIB * MIB).max(MIN_MEMORY),
            _ => FALLBACK_MEMORY,
        };
        let pids = (memory / MIB * PIDS_PER_MIB).clamp(MIN_PIDS, MAX_PIDS);
        Self { memory, pids }
    }

    /// Teto desta máquina, lido uma vez por processo de `/proc/meminfo`. O daemon do Docker roda
    /// na mesma máquina (ou, no WSL, na mesma VM) que o Nants.
    pub fn host() -> Self {
        static HOST: OnceLock<ContainerLimits> = OnceLock::new();
        *HOST.get_or_init(|| Self::from_mem_total(std::fs::read_to_string("/proc/meminfo").ok().as_deref().and_then(parse_mem_total)))
    }

    /// Memória em MiB (para as mensagens).
    pub fn memory_mib(&self) -> u64 {
        self.memory / MIB
    }

    /// Limites de um serviço do `compose.yaml`: o menor entre o que ele declara e o teto `self`.
    /// Ausente, zero, `-1` ou ilegível contam como "sem limite" e viram o teto.
    pub fn effective(&self, service: Option<&Value>) -> Self {
        let get = |path: &[&str]| path.iter().try_fold(service?, |v, k| v.get(*k));
        let memory = [get(&["mem_limit"]), get(&["deploy", "resources", "limits", "memory"])]
            .into_iter()
            .flatten()
            .filter_map(parse_bytes)
            .fold(self.memory, u64::min);
        let pids = [get(&["pids_limit"]), get(&["deploy", "resources", "limits", "pids"])]
            .into_iter()
            .flatten()
            .filter_map(parse_count)
            .fold(self.pids, u64::min);
        Self { memory, pids }
    }

    /// Chaves que o override do Nants grava no serviço para aplicar os limites efetivos de
    /// `service` (o `compose.yaml` do projeto). O Compose junta o override por cima do arquivo do
    /// projeto, então estes valores vencem os dele:
    /// - `mem_limit` e `memswap_limit` iguais (nada de swap além da memória: no incidente a swap
    ///   encheu e o host caiu junto);
    /// - `pids_limit`;
    /// - `oom_kill_disable: false` (o projeto não desliga o encerramento por falta de memória);
    /// - `mem_reservation` e as reservas do `deploy` acima do limite descem até ele (o Docker recusa
    ///   reserva maior que o limite);
    /// - os limites do `deploy.resources.limits` que o projeto declarou recebem o mesmo valor (o
    ///   Compose recusa `mem_limit` e `deploy.resources.limits.memory` diferentes).
    pub fn override_keys(&self, service: Option<&Value>) -> Mapping {
        let eff = self.effective(service);
        let mut out = Mapping::new();
        out.insert("mem_limit".into(), Value::Number(eff.memory.into()));
        out.insert("memswap_limit".into(), Value::Number(eff.memory.into()));
        out.insert("pids_limit".into(), Value::Number(eff.pids.into()));
        out.insert("oom_kill_disable".into(), Value::Bool(false));
        let svc = |path: &[&str]| path.iter().try_fold(service?, |v, k| v.get(*k));
        let above = |v: Option<&Value>| v.is_some_and(|v| parse_bytes(v).is_none_or(|b| b > eff.memory));
        if above(svc(&["mem_reservation"])) {
            out.insert("mem_reservation".into(), Value::Number(eff.memory.into()));
        }
        // No `deploy`, a memória é texto (exigência do esquema do Compose).
        let mut limits = Mapping::new();
        if svc(&["deploy", "resources", "limits", "memory"]).is_some() {
            limits.insert("memory".into(), Value::String(eff.memory.to_string()));
        }
        if svc(&["deploy", "resources", "limits", "pids"]).is_some() {
            limits.insert("pids".into(), Value::Number(eff.pids.into()));
        }
        let mut reservations = Mapping::new();
        if above(svc(&["deploy", "resources", "reservations", "memory"])) {
            reservations.insert("memory".into(), Value::String(eff.memory.to_string()));
        }
        let mut resources = Mapping::new();
        if !limits.is_empty() {
            resources.insert("limits".into(), Value::Mapping(limits));
        }
        if !reservations.is_empty() {
            resources.insert("reservations".into(), Value::Mapping(reservations));
        }
        if !resources.is_empty() {
            let mut deploy = Mapping::new();
            deploy.insert("resources".into(), Value::Mapping(resources));
            out.insert("deploy".into(), Value::Mapping(deploy));
        }
        out
    }

    /// Argumentos do `docker run` com os mesmos limites (containers fora do Compose, como o dos
    /// testes de navegador).
    pub fn docker_run_args(&self) -> Vec<String> {
        vec![
            format!("--memory={}", self.memory),
            format!("--memory-swap={}", self.memory),
            format!("--pids-limit={}", self.pids),
        ]
    }
}

/// `MemTotal` de `/proc/meminfo`, em bytes.
pub fn parse_mem_total(meminfo: &str) -> Option<u64> {
    let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.trim_start_matches("MemTotal:").trim().trim_end_matches("kB").trim().parse().ok()?;
    kb.checked_mul(1024)
}

/// Tamanho em bytes como o Compose aceita: número (bytes) ou texto com unidade binária opcional
/// (`512m`, `1.5g`, `2GiB`, `100mb`). `None` para zero, negativo ou ilegível.
pub fn parse_bytes(v: &Value) -> Option<u64> {
    let text = match v {
        Value::Number(n) => return n.as_u64().filter(|b| *b > 0).or_else(|| n.as_f64().filter(|f| *f >= 1.0).map(|f| f as u64)),
        Value::String(s) => s.trim().to_ascii_lowercase(),
        _ => return None,
    };
    let split = text.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(text.len());
    let (num, unit) = text.split_at(split);
    let num: f64 = num.parse().ok()?;
    let unit = unit.trim();
    let unit = unit.strip_suffix("ib").or_else(|| unit.strip_suffix('b')).unwrap_or(unit);
    let mult: u64 = match unit {
        "" => 1,
        "k" => 1024,
        "m" => MIB,
        "g" => 1024 * MIB,
        "t" => 1024 * 1024 * MIB,
        "p" => 1024 * 1024 * 1024 * MIB,
        _ => return None,
    };
    let bytes = num * mult as f64;
    (bytes >= 1.0 && bytes.is_finite()).then_some(bytes as u64)
}

/// Contagem positiva (número ou texto); zero, negativo ou ilegível dão `None` ("sem limite").
fn parse_count(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
    .filter(|n| *n > 0)
}

/// Qual limite do container um comando atingiu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitHit {
    /// Erro de falta de memória na saída.
    Memory,
    /// Só o código 137 (SIGKILL): quase sempre o limite de memória, mas um `kill -9` dá o mesmo.
    LikelyMemory,
    /// O sistema recusou criar processo ou thread (limite de pids).
    Pids,
}

/// Código de saída de um processo encerrado por SIGKILL (128 + 9), o que o encerramento por falta
/// de memória dentro do container produz.
pub const SIGKILL_EXIT: i32 = 137;

/// Trechos (em minúsculas) da recusa de criar processo ou thread, nas ferramentas mais comuns:
/// shells (`can't fork`, `fork: retry`), Perl (`cannot fork`), Node (`spawn ... eagain`), Java
/// (`unable to create native thread`), Rust (`failed to spawn thread`) e a libc (`pthread_create`).
const FORK_REFUSED: &[&str] = &[
    "can't fork",
    "cannot fork",
    "fork failed",
    "fork: retry",
    "fork: resource temporarily unavailable",
    "unable to create native thread",
    "unable to create new native thread",
    "failed to spawn thread",
];

/// Trechos (em minúsculas) de falta de memória nas ferramentas comuns.
const OUT_OF_MEMORY: &[&str] = &[
    "out of memory",
    "cannot allocate memory",
    "memoryerror",
    "std::bad_alloc",
    "memory allocation of",
];

/// O comando que terminou (sem estourar o tempo) morreu por um limite do container? Só olha
/// comandos que falharam. A recusa de processo vem antes: um fork bomb também pode acabar em 137.
pub fn detect_hit(code: Option<i32>, timed_out: bool, output: &str) -> Option<LimitHit> {
    if timed_out || code == Some(0) {
        return None;
    }
    let lower = output.to_ascii_lowercase();
    let fork_refused = FORK_REFUSED.iter().any(|p| lower.contains(p))
        || lower.lines().any(|l| {
            (l.contains("resource temporarily unavailable") || l.contains("eagain"))
                && ["fork", "spawn", "thread", "process"].iter().any(|w| l.contains(w))
        });
    if fork_refused {
        return Some(LimitHit::Pids);
    }
    if OUT_OF_MEMORY.iter().any(|p| lower.contains(p)) {
        return Some(LimitHit::Memory);
    }
    (code == Some(SIGKILL_EXIT)).then_some(LimitHit::LikelyMemory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(text: &str) -> Value {
        serde_yaml::from_str(text).expect("yaml")
    }

    #[test]
    fn host_cap_is_half_the_ram_with_pids_from_memory() {
        // A máquina do incidente: 7,6 GB.
        let l = ContainerLimits::from_mem_total(Some(7_992_980 * 1024));
        assert_eq!(l.memory_mib(), 3902);
        assert_eq!(l.memory % MIB, 0);
        assert_eq!(l.pids, 3902);
        // Máquina grande: a memória acompanha, os processos param no teto.
        let big = ContainerLimits::from_mem_total(Some(64 * 1024 * MIB));
        assert_eq!((big.memory_mib(), big.pids), (32 * 1024, MAX_PIDS));
        // Máquina pequena: pisos.
        let small = ContainerLimits::from_mem_total(Some(700 * MIB));
        assert_eq!((small.memory, small.pids), (MIN_MEMORY, MIN_PIDS));
        // Sem /proc/meminfo: o valor de reserva.
        assert_eq!(ContainerLimits::from_mem_total(None).memory, FALLBACK_MEMORY);
        assert_eq!(ContainerLimits::host(), ContainerLimits::host());
    }

    #[test]
    fn reads_mem_total() {
        assert_eq!(parse_mem_total("MemTotal:        7992980 kB\nMemFree: 1 kB\n"), Some(7_992_980 * 1024));
        assert_eq!(parse_mem_total("MemFree: 1 kB\n"), None);
    }

    #[test]
    fn parses_compose_sizes() {
        let p = |t: &str| parse_bytes(&yaml(t));
        assert_eq!(p("512m"), Some(512 * MIB));
        assert_eq!(p("\"1.5g\""), Some(1536 * MIB));
        assert_eq!(p("2GiB"), Some(2048 * MIB));
        assert_eq!(p("100mb"), Some(100 * MIB));
        assert_eq!(p("1048576"), Some(MIB));
        assert_eq!(p("\"1048576b\""), Some(MIB));
        assert_eq!(p("0"), None);
        assert_eq!(p("-1"), None);
        assert_eq!(p("muito"), None);
        assert_eq!(p("12x"), None);
    }

    #[test]
    fn smaller_project_value_wins_and_larger_or_missing_becomes_the_cap() {
        let cap = ContainerLimits { memory: 4096 * MIB, pids: 2048 };
        assert_eq!(cap.effective(None), cap);
        let small = yaml("mem_limit: 512m\npids_limit: 100\n");
        assert_eq!(cap.effective(Some(&small)), ContainerLimits { memory: 512 * MIB, pids: 100 });
        let big = yaml("mem_limit: 64g\npids_limit: 100000\n");
        assert_eq!(cap.effective(Some(&big)), cap);
        // "Sem limite" declarado (-1, 0) não desliga o teto.
        let off = yaml("mem_limit: -1\npids_limit: -1\n");
        assert_eq!(cap.effective(Some(&off)), cap);
        let zero = yaml("mem_limit: 0\npids_limit: 0\n");
        assert_eq!(cap.effective(Some(&zero)), cap);
        // O `deploy` também conta, e o menor dos dois vale.
        let deploy = yaml("mem_limit: 3g\ndeploy:\n  resources:\n    limits:\n      memory: 1G\n      pids: 300\n");
        assert_eq!(cap.effective(Some(&deploy)), ContainerLimits { memory: 1024 * MIB, pids: 300 });
    }

    #[test]
    fn override_keys_cannot_be_switched_off_by_the_project() {
        let cap = ContainerLimits { memory: 4096 * MIB, pids: 2048 };
        let svc = yaml(
            "mem_limit: 8g\nmemswap_limit: -1\nmem_reservation: 10g\npids_limit: -1\noom_kill_disable: true\n\
             deploy:\n  resources:\n    limits:\n      cpus: '2'\n      memory: 8G\n      pids: 100000\n    reservations:\n      memory: 9G\n",
        );
        let ov = Value::Mapping(cap.override_keys(Some(&svc)));
        let m = 4096 * MIB;
        assert_eq!(ov["mem_limit"].as_u64(), Some(m));
        assert_eq!(ov["memswap_limit"].as_u64(), Some(m));
        assert_eq!(ov["mem_reservation"].as_u64(), Some(m));
        assert_eq!(ov["pids_limit"].as_u64(), Some(2048));
        assert_eq!(ov["oom_kill_disable"].as_bool(), Some(false));
        assert_eq!(ov["deploy"]["resources"]["limits"]["memory"].as_str(), Some(m.to_string().as_str()));
        assert_eq!(ov["deploy"]["resources"]["limits"]["pids"].as_u64(), Some(2048));
        assert_eq!(ov["deploy"]["resources"]["reservations"]["memory"].as_str(), Some(m.to_string().as_str()));
        // O `cpus` do projeto não é tocado (o Compose junta o `deploy` por chave).
        assert!(ov["deploy"]["resources"]["limits"].get("cpus").is_none());

        // Serviço sem nada: só as chaves de topo, sem `deploy` (o Compose antigo pode não conhecer `pids`).
        let plain = Value::Mapping(cap.override_keys(Some(&yaml("image: alpine\n"))));
        assert_eq!(plain["mem_limit"].as_u64(), Some(m));
        assert!(plain.get("deploy").is_none() && plain.get("mem_reservation").is_none());

        // Valor menor do projeto continua valendo, e a reserva menor fica como está.
        let low = Value::Mapping(cap.override_keys(Some(&yaml("mem_limit: 256m\nmem_reservation: 128m\npids_limit: 50\n"))));
        assert_eq!((low["mem_limit"].as_u64(), low["pids_limit"].as_u64()), (Some(256 * MIB), Some(50)));
        assert!(low.get("mem_reservation").is_none());
    }

    #[test]
    fn docker_run_args_carry_the_same_limits() {
        let cap = ContainerLimits { memory: 256 * MIB, pids: 64 };
        assert_eq!(cap.docker_run_args(), ["--memory=268435456", "--memory-swap=268435456", "--pids-limit=64"]);
    }

    #[test]
    fn detects_memory_and_process_limits() {
        assert_eq!(detect_hit(Some(137), false, "Killed\n"), Some(LimitHit::LikelyMemory));
        assert_eq!(detect_hit(Some(137), false, "out of memory\n"), Some(LimitHit::Memory));
        assert_eq!(detect_hit(Some(1), false, "FATAL ERROR: Reached heap limit JavaScript heap out of memory"), Some(LimitHit::Memory));
        assert_eq!(detect_hit(Some(2), false, "sh: can't fork: Resource temporarily unavailable"), Some(LimitHit::Pids));
        assert_eq!(detect_hit(Some(255), false, "Cannot fork: Resource temporarily unavailable at t/runner.pl line 3."), Some(LimitHit::Pids));
        assert_eq!(detect_hit(Some(1), false, "Error: spawn /bin/sh EAGAIN"), Some(LimitHit::Pids));
        assert_eq!(detect_hit(Some(1), false, "bash: fork: retry: Resource temporarily unavailable"), Some(LimitHit::Pids));
        assert_eq!(detect_hit(Some(1), false, "java.lang.OutOfMemoryError: unable to create native thread"), Some(LimitHit::Pids));
        // Fork recusado que termina em 137 é dos processos.
        assert_eq!(detect_hit(Some(137), false, "sh: can't fork"), Some(LimitHit::Pids));
        // Tempo esgotado, sucesso e falha comum não são limite.
        assert_eq!(detect_hit(None, true, "can't fork"), None);
        assert_eq!(detect_hit(Some(0), false, "out of memory? no"), None);
        assert_eq!(detect_hit(Some(1), false, "1 failed, 3 passed"), None);
    }
}
