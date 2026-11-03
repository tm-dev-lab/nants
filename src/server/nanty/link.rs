//! O caminho até o Nants (spec 140 T03, C-30): o pedido de abrir vira efeito de ferramenta, e o
//! servidor acrescenta o resultado à resposta, como a confirmação do silêncio. O modelo nunca escreve
//! o link: o `format::clean` o neutralizaria, e um link vindo dele não é confiável.
//!
//! - **Endereço:** o campo opcional dos Ajustes ([`parse_address`]) ou, em modo rede, o último `Host`
//!   aceito vindo de um aparelho autorizado ([`device_host`]). O servidor não adivinha: no WSL, a
//!   interface local não é o IP que o celular alcança.
//! - **Só o endereço da tela:** `/` ou `/?space=<projeto>`, sem token e sem link de entrada. O aparelho
//!   sem autorização cai na tela de conexão de sempre.
//! - **Por canal ([`Open`]):** no Telegram, o link; sem endereço, a frase que manda abrir no computador.
//!   Na janela, a tela do projeto, sem link.
//! - **O link fica fora do texto:** ele vai em `params`, longe do `clean` e da fala. A fila de saída o
//!   põe no botão, ou no texto se o Telegram recusar o botão.

use serde_json::{json, Map, Value};

use super::store::{Channel, ConversationMessage};
use super::{tools, turn, Nanty};
use crate::config::{Config, Project};
use crate::i18n::Lang;
use crate::server::events::SecretSet;
use crate::server::state::AppState;

/// Parâmetro da mensagem com o link que a fila de saída manda ao Telegram.
pub const OPEN_URL: &str = "open_url";
/// Parâmetro da mensagem com o projeto que a janela abre.
pub const OPEN_PROJECT: &str = "open_project";
/// Maior endereço aceito nos Ajustes, em bytes.
const MAX_ADDRESS: usize = 200;

/// O que o pedido de abrir o Nants entrega à pessoa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Open {
    /// Telegram: o link, no botão ou no texto.
    Link(String),
    /// Janela: a tela do projeto.
    Screen { project_id: String, name: String },
    /// Janela, sem projeto: ela já está no Nants.
    Here,
    /// Telegram, sem endereço que o aparelho alcance: abrir no computador.
    Computer,
}

impl Open {
    /// A frase da mensagem, no dicionário.
    pub fn code(&self) -> &'static str {
        match self {
            Open::Link(_) => "nanty.chat.open_link",
            Open::Screen { .. } => "nanty.chat.open_screen",
            Open::Here => "nanty.chat.open_here",
            Open::Computer => "nanty.chat.open_computer",
        }
    }

    /// O resultado da ferramenta: o que a pessoa vai receber, sem o endereço.
    pub fn tool_code(&self) -> &'static str {
        match self {
            Open::Link(_) => "nanty.tool.open_link",
            Open::Screen { .. } => "nanty.tool.open_screen",
            Open::Here => "nanty.tool.open_here",
            Open::Computer => "nanty.tool.open_computer",
        }
    }

    /// Os parâmetros da frase e o caminho, para a fila de saída e a janela.
    pub fn params(&self) -> Map<String, Value> {
        let mut out = Map::new();
        match self {
            Open::Link(url) => {
                out.insert(OPEN_URL.into(), json!(url));
            }
            Open::Screen { project_id, name } => {
                out.insert("project".into(), json!(name));
                out.insert(OPEN_PROJECT.into(), json!(project_id));
            }
            Open::Here | Open::Computer => {}
        }
        out
    }
}

/// O IP sem colchetes e na forma canônica, se o texto for um.
fn ip_of(name: &str) -> Option<std::net::IpAddr> {
    name.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>().ok().map(|ip| ip.to_canonical())
}

/// O `Host` de um aparelho, se servir de endereço: um IP da rede. Nome (que a guarda de `Host` não
/// aceita), loopback e o endereço vazio ficam de fora. Só vale chamar com a requisição já conferida
/// pela guarda de `Host`/`Origin` e pelo dispositivo ([`crate::server::devices::require_device`]).
pub fn device_host(host: &str) -> Option<String> {
    let ip = ip_of(&crate::server::app::host_name(host))?;
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| host.to_ascii_lowercase())
}

/// O endereço dos Ajustes na forma guardada, `esquema://nome[:porta]`. Sem esquema, vale `http`. Fora
/// disso (usuário e senha, caminho, consulta, outro esquema ou loopback), `None`: um token ou um link
/// de entrada nunca ficam aqui.
pub fn parse_address(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() > MAX_ADDRESS {
        return None;
    }
    let text = if raw.contains("://") { raw.to_string() } else { format!("http://{raw}") };
    let url = reqwest::Url::parse(&text).ok()?;
    let plain = matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none();
    let host = url.host_str().filter(|_| plain)?;
    let local = host.eq_ignore_ascii_case("localhost") || ip_of(host).is_some_and(|ip| ip.is_loopback() || ip.is_unspecified());
    if local {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// O endereço que o aparelho alcança: o dos Ajustes ou, em modo rede, o do último aparelho.
pub fn address(state: &AppState, cfg: &Config) -> Option<String> {
    if !cfg.nanty.address.is_empty() {
        return Some(cfg.nanty.address.clone());
    }
    state.lan_mode().then(|| state.nanty().snapshot().device_host).flatten().map(|host| format!("http://{host}"))
}

/// A tela: o começo do Nants ou o espaço do projeto, sem token e sem link de entrada.
fn page_url(base: &str, project_id: Option<&str>) -> String {
    match project_id.filter(|id| turn::is_code(id)) {
        Some(id) => format!("{base}/?space={id}"),
        None => format!("{base}/"),
    }
}

/// O que o pedido entrega neste canal.
pub fn resolve(state: &AppState, cfg: &Config, secrets: &SecretSet, channel: Channel, project: Option<&Project>) -> Open {
    match channel {
        Channel::Web => match project {
            Some(p) => Open::Screen { project_id: p.id.clone(), name: tools::project_name(secrets, p) },
            None => Open::Here,
        },
        Channel::Telegram => match address(state, cfg) {
            Some(base) => Open::Link(page_url(&base, project.map(|p| p.id.as_str()))),
            None => Open::Computer,
        },
    }
}

/// O link da mensagem, para a fila de saída.
pub fn url_of(message: &ConversationMessage) -> Option<&str> {
    message.params.get(OPEN_URL).and_then(Value::as_str)
}

/// O botão do cartão que leva ao Nants foi tocado no Telegram: o caminho sai como mensagem. Responde
/// ao toque (`reply_to`, o cartão): não espera a aba visível, como a resposta a quem escreveu.
pub async fn send_to_telegram(state: &AppState, nanty: &std::sync::Arc<Nanty>, lang: Lang, card_id: &str, project_id: Option<&str>) {
    let cfg = state.config().read().await.clone();
    let secrets = SecretSet::from_config(&cfg);
    let project = project_id.and_then(|id| cfg.projects.iter().find(|p| p.id == id && cfg.nanty.observes(id)));
    let open = resolve(state, &cfg, &secrets, Channel::Telegram, project);
    let mut message = ConversationMessage::from_nanty(super::new_id(), nanty.now().epoch_ms, Channel::Telegram, open.code(), open.params());
    message.lang = Some(lang.tag().to_string());
    message.reply_to = Some(card_id.to_string());
    let n = std::sync::Arc::clone(nanty);
    let _ = tokio::task::spawn_blocking(move || n.push_message(&message)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_network_ip_counts_as_a_device_address() {
        assert_eq!(device_host("192.168.0.68:3000").as_deref(), Some("192.168.0.68:3000"));
        assert_eq!(device_host("[fd00::1]:3000").as_deref(), Some("[fd00::1]:3000"));
        for host in ["localhost:3000", "127.0.0.1:3000", "127.9.9.9:3000", "[::1]:3000", "[::ffff:127.0.0.1]:3000", "0.0.0.0:3000", "evil.example:3000", ""] {
            assert_eq!(device_host(host), None, "{host}");
        }
    }

    #[test]
    fn the_settings_address_is_only_scheme_host_and_port() {
        assert_eq!(parse_address("http://192.168.0.68:3000").as_deref(), Some("http://192.168.0.68:3000"));
        assert_eq!(parse_address(" 192.168.0.68:3000/ ").as_deref(), Some("http://192.168.0.68:3000"));
        assert_eq!(parse_address("https://nants.example.com").as_deref(), Some("https://nants.example.com"));
        assert_eq!(parse_address("https://nants.example.com:8443/").as_deref(), Some("https://nants.example.com:8443"));
        for bad in [
            "localhost:3000",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
            "http://0.0.0.0:3000",
            "ftp://nants.example.com",
            "tg://resolve?domain=x",
            "javascript:alert(1)",
            "http://user:senha@nants.example.com",
            "http://nants.example.com/pair?token=abc",
            "http://nants.example.com/connect",
            "http://nants.example.com/#x",
            "nants example",
            "",
        ] {
            assert_eq!(parse_address(bad), None, "{bad}");
        }
        assert_eq!(parse_address(&format!("http://{}.example.com", "a".repeat(MAX_ADDRESS))), None);
    }

    #[test]
    fn the_page_has_no_token_and_no_entry_link() {
        assert_eq!(page_url("http://10.0.0.2:3000", None), "http://10.0.0.2:3000/");
        assert_eq!(page_url("http://10.0.0.2:3000", Some("p1")), "http://10.0.0.2:3000/?space=p1");
        assert_eq!(page_url("http://10.0.0.2:3000", Some("a b&x=1")), "http://10.0.0.2:3000/", "id fora da forma de código fica de fora");
    }
}
