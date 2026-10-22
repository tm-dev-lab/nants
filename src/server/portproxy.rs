//! Redirecionamento de portas do Windows para o WSL (spec 111), só leitura.
//!
//! O script de Configurações > Acesso pela rede (spec 097/099) cria `netsh interface portproxy`
//! de `0.0.0.0:3000-3010` do Windows para o IP do eth0 do WSL. Esse redirecionamento tem
//! prioridade sobre o encaminhamento de localhost do WSL: no modo local (só 127.0.0.1) o navegador
//! do Windows não abre a página. Aqui o Nants só confere, com
//! `netsh.exe interface portproxy show v4tov4` e tempo limite curto; nada é alterado no Windows.
//!
//! A saída do `netsh` muda com o idioma do Windows ("Escuta em ipv4", "Listen on ipv4") e pode vir
//! em CP850 ou Latin-1. O parser ignora os cabeçalhos e procura as linhas de dados: endereço,
//! porta, endereço e porta. A porta conferida é a da coluna de escuta.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::prereqs::{CmdOutput, CmdRunner};

/// Tempo limite de cada consulta ao Windows (`netsh.exe`, `powershell.exe`, `wslinfo`). As
/// consultas do início rodam em paralelo: o pior caso fica abaixo de 3 s (spec 111, critério 6).
pub const NETSH_TIMEOUT: Duration = Duration::from_millis(2500);
/// Argumentos da consulta (só `show`; nunca `add`, `set` ou `delete`).
pub const NETSH_ARGS: [&str; 4] = ["interface", "portproxy", "show", "v4tov4"];
/// Onde procurar o `netsh.exe`: pelo PATH da interop e, sem ele no PATH, no caminho padrão.
pub const NETSH_PROGRAMS: [&str; 2] = ["netsh.exe", "/mnt/c/Windows/System32/netsh.exe"];

/// O programa existe? Nome com `/` é um caminho; sem `/`, procura no `PATH`.
pub type ProgramExists = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Executor e localizador dos programas do Windows (injetáveis nos testes). O localizador decide o
/// programa antes de executar: a alternativa só é tentada quando o primeiro não existe, nunca
/// depois de um tempo esgotado (um `netsh` travado custa um prazo, não dois).
#[derive(Clone)]
pub struct WinTools {
    pub run: CmdRunner,
    pub exists: ProgramExists,
}

impl std::fmt::Debug for WinTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WinTools")
    }
}

impl WinTools {
    /// Executor real com [`NETSH_TIMEOUT`] por consulta e busca real no `PATH`.
    pub fn real() -> Self {
        Self { run: super::prereqs::real_runner(NETSH_TIMEOUT), exists: Arc::new(program_exists) }
    }

    /// Primeiro candidato que existe.
    pub fn resolve<'a>(&self, candidates: &[&'a str]) -> Option<&'a str> {
        candidates.iter().copied().find(|c| (self.exists)(c))
    }

    /// Executa só o primeiro candidato que existe; `None` sem nenhum, com tempo esgotado ou erro.
    pub fn run_first(&self, candidates: &[&str], args: &[&str]) -> Option<CmdOutput> {
        let program = self.resolve(candidates)?;
        (self.run)(program, args)
    }
}

/// Busca real: caminho com `/` precisa ser um arquivo; nome simples, um arquivo em alguma pasta do
/// `PATH`.
pub fn program_exists(name: &str) -> bool {
    if name.contains('/') {
        return Path::new(name).is_file();
    }
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
}

/// Resultado da conferência.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectState {
    /// O Windows redireciona a porta para algum endereço (o modo local fica inacessível).
    Redirected,
    /// Nenhum redirecionamento na porta (ou fora do WSL, onde a questão não existe).
    NotRedirected,
    /// Não deu para conferir: sem interop, sem `netsh.exe`, tempo esgotado, erro ou saída
    /// irreconhecível. Nunca é tratado como erro.
    Unknown,
}

impl RedirectState {
    /// Forma da API: `yes`, `no` ou `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            RedirectState::Redirected => "yes",
            RedirectState::NotRedirected => "no",
            RedirectState::Unknown => "unknown",
        }
    }
}

/// Uma linha da tabela do `netsh`: porta de escuta no Windows e endereço de destino.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub listen_port: u16,
    pub connect_addr: String,
}

/// Endereço da tabela do `netsh`: IPv4, `*` ou nome de máquina.
fn is_address(token: &str) -> bool {
    !token.is_empty()
        && token.chars().any(|c| c.is_ascii_alphanumeric() || c == '*')
        && token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '*' | ':'))
        && token.parse::<u16>().is_err()
}

/// Linha separadora do cabeçalho (`----- -----`).
fn is_separator(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| c == '-' || c.is_whitespace())
}

/// Redirecionamentos listados na saída do `netsh ... show`. `None` quando a saída não é
/// reconhecida (texto sem tabela e sem linhas de dados). Saída vazia é uma lista vazia: o `netsh`
/// não imprime nada quando não há redirecionamentos.
pub fn redirects(output: &str) -> Option<Vec<Redirect>> {
    if output.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut found = Vec::new();
    let mut table = false;
    for line in output.lines() {
        if is_separator(line) {
            table = true;
            continue;
        }
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let [listen_addr, listen_port, connect_addr, connect_port] = tokens.as_slice() else {
            continue;
        };
        let (Ok(port), Ok(_)) = (listen_port.parse::<u16>(), connect_port.parse::<u16>()) else {
            continue;
        };
        if is_address(listen_addr) && is_address(connect_addr) {
            found.push(Redirect { listen_port: port, connect_addr: (*connect_addr).to_string() });
        }
    }
    (table || !found.is_empty()).then_some(found)
}

/// Portas de escuta listadas na saída do `netsh ... show` (ver [`redirects`]).
pub fn listen_ports(output: &str) -> Option<Vec<u16>> {
    redirects(output).map(|list| list.into_iter().map(|r| r.listen_port).collect())
}

/// Tabela pela saída do comando (`None` = não executou, falhou ou saída irreconhecível).
pub fn table_from_output(out: Option<CmdOutput>) -> Option<Vec<Redirect>> {
    out.filter(|o| o.success).and_then(|o| redirects(&o.stdout))
}

/// Estado de uma porta pela tabela (`None` = não deu para conferir).
pub fn state_in(table: Option<&[Redirect]>, port: u16) -> RedirectState {
    match table {
        Some(list) if list.iter().any(|r| r.listen_port == port) => RedirectState::Redirected,
        Some(_) => RedirectState::NotRedirected,
        None => RedirectState::Unknown,
    }
}

/// Estado pela saída do comando (`None` = não executou ou tempo esgotado).
pub fn state_from_output(out: Option<CmdOutput>, port: u16) -> RedirectState {
    state_in(table_from_output(out).as_deref(), port)
}

/// Lê a tabela do Windows. Fora do WSL não executa nada e devolve a lista vazia.
pub fn read_table(tools: &WinTools, wsl: bool) -> Option<Vec<Redirect>> {
    if !wsl {
        return Some(Vec::new());
    }
    table_from_output(tools.run_first(&NETSH_PROGRAMS, &NETSH_ARGS))
}

/// Confere a porta com as ferramentas dadas. Fora do WSL não executa nada; porta 0 (escolhida
/// pelo sistema ao iniciar) não dá para conferir.
pub fn check_with(tools: &WinTools, wsl: bool, port: u16) -> RedirectState {
    if !wsl {
        return RedirectState::NotRedirected;
    }
    if port == 0 {
        return RedirectState::Unknown;
    }
    state_in(read_table(tools, wsl).as_deref(), port)
}

/// O Windows redireciona a `port` para o WSL? Bloqueante, com tempo limite de
/// [`NETSH_TIMEOUT`]; no código assíncrono, chame dentro de `spawn_blocking`.
pub fn windows_redirects_port(port: u16) -> RedirectState {
    check_with(&WinTools::real(), super::netinfo::running_on_wsl(), port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Saída real do Windows em português (CRLF).
    const PT: &str = "\r\nEscuta em ipv4:             Conectar-se a ipv4:\r\n\r\nEndereço        Porta       Endereço        Porta\r\n--------------- ----------  --------------- ----------\r\n0.0.0.0         3000        172.21.154.252  3000\r\n0.0.0.0         3001        172.21.154.252  3001\r\n";
    const EN: &str = "\r\nListen on ipv4:             Connect to ipv4:\r\n\r\nAddress         Port        Address         Port\r\n--------------- ----------  --------------- ----------\r\n*               8080        localhost       80\r\n192.168.0.10    3000        172.21.154.252  3000\r\n";

    fn ok(s: &str) -> Option<CmdOutput> {
        Some(CmdOutput { success: true, stdout: s.into(), stderr: String::new() })
    }

    fn tools(run: CmdRunner, present: &'static [&'static str]) -> WinTools {
        WinTools { run, exists: Arc::new(move |p| present.contains(&p)) }
    }

    #[test]
    fn parses_the_real_portuguese_output_and_english() {
        assert_eq!(listen_ports(PT), Some(vec![3000, 3001]));
        assert_eq!(listen_ports(EN), Some(vec![8080, 3000]));
        let table = redirects(EN).expect("tabela");
        assert_eq!(table[0], Redirect { listen_port: 8080, connect_addr: "localhost".into() });
        assert_eq!(table[1].connect_addr, "172.21.154.252");
        assert_eq!(state_from_output(ok(PT), 3000), RedirectState::Redirected);
        assert_eq!(state_from_output(ok(PT), 3001), RedirectState::Redirected);
        assert_eq!(state_from_output(ok(PT), 3002), RedirectState::NotRedirected);
        assert_eq!(state_from_output(ok(EN), 3000), RedirectState::Redirected);
        // A porta de destino não conta: só a coluna de escuta.
        assert_eq!(state_from_output(ok(EN), 80), RedirectState::NotRedirected);
    }

    #[test]
    fn reads_cp850_and_latin1_without_utf8() {
        // "Endereço" com o ç em CP850 (0x87) e em Latin-1 (0xE7), lido sem exigir UTF-8.
        for c in [0x87u8, 0xE7] {
            let mut bytes = PT.replace('ç', "\u{1}").into_bytes();
            for b in bytes.iter_mut().filter(|b| **b == 1) {
                *b = c;
            }
            assert!(std::str::from_utf8(&bytes).is_err());
            let text = String::from_utf8_lossy(&bytes);
            assert_eq!(state_from_output(ok(&text), 3000), RedirectState::Redirected);
        }
    }

    #[test]
    fn empty_header_only_and_strange_outputs() {
        assert_eq!(state_from_output(ok(""), 3000), RedirectState::NotRedirected);
        assert_eq!(state_from_output(ok("\r\n\r\n"), 3000), RedirectState::NotRedirected);
        let header_only = "Listen on ipv4:  Connect to ipv4:\r\n\r\nAddress Port Address Port\r\n------- ---- ------- ----\r\n";
        assert_eq!(state_from_output(ok(header_only), 3000), RedirectState::NotRedirected);
        for strange in ["O comando a seguir não foi encontrado: interface portproxy show v4tov4.", "3000 3000 3000 3000", "a b c d"] {
            assert_eq!(state_from_output(ok(strange), 3000), RedirectState::Unknown, "{strange}");
        }
        let failed = Some(CmdOutput { success: false, stdout: PT.into(), stderr: "erro".into() });
        assert_eq!(state_from_output(failed, 3000), RedirectState::Unknown);
        assert_eq!(state_from_output(None, 3000), RedirectState::Unknown);
    }

    #[test]
    fn only_reads_and_only_on_wsl() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let run: CmdRunner = Arc::new(move |prog, args| {
            seen.fetch_add(1, Ordering::SeqCst);
            assert_eq!(args, NETSH_ARGS, "só show");
            (prog == "netsh.exe").then(|| ok(PT)).flatten()
        });
        let on_path = tools(run, &["netsh.exe", "/mnt/c/Windows/System32/netsh.exe"]);
        assert_eq!(check_with(&on_path, false, 3000), RedirectState::NotRedirected);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "fora do WSL não executa");
        assert_eq!(check_with(&on_path, true, 0), RedirectState::Unknown);
        assert_eq!(check_with(&on_path, true, 3000), RedirectState::Redirected);
        assert_eq!(check_with(&on_path, true, 4000), RedirectState::NotRedirected);
        // Sem netsh.exe no PATH, usa o caminho padrão; sem nenhum, não executa e não deu para conferir.
        let fallback = tools(Arc::new(|prog, _| (prog != "netsh.exe").then(|| ok(PT)).flatten()), &["/mnt/c/Windows/System32/netsh.exe"]);
        assert_eq!(check_with(&fallback, true, 3001), RedirectState::Redirected);
        let absent = tools(Arc::new(|_, _| panic!("nada a executar")), &[]);
        assert_eq!(check_with(&absent, true, 3000), RedirectState::Unknown);
    }

    #[test]
    fn a_timeout_never_tries_the_fallback() {
        // Programa encontrado que esgota o prazo (o executor devolve `None`): uma tentativa só.
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = calls.clone();
        let run: CmdRunner = Arc::new(move |_, _| {
            seen.fetch_add(1, Ordering::SeqCst);
            None
        });
        let stuck = tools(run, &["netsh.exe", "/mnt/c/Windows/System32/netsh.exe"]);
        assert_eq!(check_with(&stuck, true, 3000), RedirectState::Unknown);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn program_lookup_uses_path_or_the_file() {
        assert!(program_exists("sh"));
        assert!(!program_exists("nants-programa-que-nao-existe.exe"));
        assert!(program_exists("/bin/sh"));
        assert!(!program_exists("/nao/existe/netsh.exe"));
    }

    #[test]
    fn real_runner_keeps_non_utf8_output() {
        // Executor real lendo bytes Latin-1 (sem shell para o Nants; o `printf` é só a fonte).
        let run = crate::server::prereqs::real_runner(NETSH_TIMEOUT);
        let out = run("printf", &["0.0.0.0 3000 172.21.154.252 3000\\nEndere\\347o\\n-- --\\n"]).expect("printf");
        assert!(out.success && out.stdout.contains("Endere"), "{out:?}");
        assert_eq!(state_from_output(Some(out), 3000), RedirectState::Redirected);
    }
}
