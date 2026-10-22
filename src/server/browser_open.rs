//! Abertura do navegador padrão ao iniciar com `nants` (spec 103, decisão 1).
//!
//! Ordem: `$BROWSER` (lista separada por `:`, com `%s` opcional para a URL); no WSL,
//! `wslview` e, na falta dele, `powershell.exe -NoProfile -Command Start-Process '<url>'`;
//! no Linux, `xdg-open`. Cada tentativa tem um tempo limite curto: se o processo não terminar
//! dentro dele, considera-se aberto e o processo segue sozinho (nunca é morto, porque pode ser o
//! próprio navegador). Quem chama roda isto numa tarefa separada, então o servidor nunca espera.

use std::time::Duration;

use tokio::process::Command;

/// Tempo máximo de espera por cada tentativa de abertura.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// Um comando candidato para abrir a URL: programa e argumentos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    pub program: String,
    pub args: Vec<String>,
}

/// Lista ordenada de comandos para abrir `url`. Função pura: recebe o valor de `$BROWSER`
/// (`None` ou vazio = ausente) e se o servidor roda no WSL.
pub fn candidates(browser_env: Option<&str>, wsl: bool, url: &str) -> Vec<Opener> {
    let mut list = Vec::new();
    for entry in browser_env.unwrap_or("").split(':') {
        let mut parts = entry.split_whitespace().map(str::to_string);
        let Some(program) = parts.next() else { continue };
        let mut args: Vec<String> = parts.collect();
        if args.iter().any(|a| a.contains("%s")) {
            args = args.into_iter().map(|a| a.replace("%s", url)).collect();
        } else {
            args.push(url.to_string());
        }
        list.push(Opener { program, args });
    }
    if wsl {
        list.push(Opener { program: "wslview".into(), args: vec![url.to_string()] });
        let quoted = url.replace('\'', "''");
        list.push(Opener {
            program: "powershell.exe".into(),
            args: vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!("Start-Process '{quoted}'"),
            ],
        });
    }
    list.push(Opener { program: "xdg-open".into(), args: vec![url.to_string()] });
    list
}

/// Resultado de uma tentativa.
#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    Opened,
    Failed,
}

async fn try_open(opener: &Opener, timeout: Duration) -> Attempt {
    let spawned = Command::new(&opener.program)
        .args(&opener.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => return Attempt::Failed,
    };
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) if status.success() => Attempt::Opened,
        Ok(_) => Attempt::Failed,
        // Ainda rodando depois do prazo: provavelmente o próprio navegador; segue sozinho.
        Err(_) => Attempt::Opened,
    }
}

/// Tenta abrir `url` com os candidatos, em ordem, até um funcionar. Devolve o programa usado.
pub async fn open_with(list: &[Opener], timeout: Duration) -> Option<String> {
    for opener in list {
        if try_open(opener, timeout).await == Attempt::Opened {
            return Some(opener.program.clone());
        }
    }
    None
}

/// Abre `url` no navegador padrão do sistema, com a ordem da spec 103.
pub async fn open_default(url: &str) -> Option<String> {
    let browser = std::env::var("BROWSER").ok();
    let list = candidates(browser.as_deref(), super::netinfo::running_on_wsl(), url);
    open_with(&list, OPEN_TIMEOUT).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "http://127.0.0.1:3000";

    fn programs(list: &[Opener]) -> Vec<&str> {
        list.iter().map(|o| o.program.as_str()).collect()
    }

    #[test]
    fn linux_without_browser_uses_xdg_open() {
        let list = candidates(None, false, URL);
        assert_eq!(list, vec![Opener { program: "xdg-open".into(), args: vec![URL.into()] }]);
        assert_eq!(programs(&candidates(Some("  "), false, URL)), vec!["xdg-open"]);
    }

    #[test]
    fn wsl_order_is_wslview_then_powershell_then_xdg_open() {
        let list = candidates(None, true, URL);
        assert_eq!(programs(&list), vec!["wslview", "powershell.exe", "xdg-open"]);
        assert_eq!(
            list[1].args,
            vec!["-NoProfile".to_string(), "-Command".into(), format!("Start-Process '{URL}'")]
        );
    }

    #[test]
    fn browser_env_comes_first_and_supports_lists_and_placeholder() {
        let list = candidates(Some("firefox --new-tab:chrome %s:"), true, URL);
        assert_eq!(programs(&list), vec!["firefox", "chrome", "wslview", "powershell.exe", "xdg-open"]);
        assert_eq!(list[0].args, vec!["--new-tab".to_string(), URL.into()]);
        assert_eq!(list[1].args, vec![URL.to_string()]);
    }

    #[test]
    fn powershell_quotes_are_escaped() {
        let list = candidates(None, true, "http://x/a'b");
        assert_eq!(list[1].args[2], "Start-Process 'http://x/a''b'");
    }

    #[tokio::test]
    async fn open_with_falls_through_missing_and_failing_programs() {
        let list = vec![
            Opener { program: "nants-programa-inexistente-xyz".into(), args: vec![] },
            Opener { program: "false".into(), args: vec![] },
            Opener { program: "true".into(), args: vec![] },
        ];
        assert_eq!(open_with(&list, Duration::from_secs(5)).await.as_deref(), Some("true"));
        assert_eq!(open_with(&list[..2], Duration::from_secs(5)).await, None);
    }

    #[tokio::test]
    async fn slow_opener_counts_as_opened_after_timeout() {
        let list = vec![Opener { program: "sleep".into(), args: vec!["5".into()] }];
        let started = std::time::Instant::now();
        assert_eq!(open_with(&list, Duration::from_millis(200)).await.as_deref(), Some("sleep"));
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
