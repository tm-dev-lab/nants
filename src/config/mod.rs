//! Configuração global do usuário (`~/.config/nants/config.toml`).

mod schema;
mod view_ports;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use schema::{
    default_image_price, AgentConfig, DEFAULT_PROJECTS_ROOT, Agents, MAX_STEPS_RANGE, Config, Connection, Effort, PriceSource, Pricing, Project, ServerConfig, WireApi,
    LanViewPortsConfig,
};
pub use view_ports::{ViewPorts, DEFAULT_VIEW_PORTS, MAX_VIEW_PORTS, MIN_VIEW_PORT, VIEW_PORTS_EXHAUSTED_PREFIX};

/// Erros da configuração global.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("erro de I/O: {0}")]
    Io(#[from] io::Error),
    #[error("erro ao interpretar TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("erro ao serializar TOML: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("variável de ambiente ausente: {0}")]
    MissingEnv(String),
    #[error("diretório home não encontrado (HOME ausente ou vazio)")]
    HomeDirNotFound,
}

impl Config {
    /// Carrega a configuração de `path`. Arquivo ausente retorna `Config::default()`
    /// sem criar nenhum arquivo.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(content) => Ok(toml::from_str(&content)?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Salva a configuração em `path`, criando os diretórios pai se necessário.
    /// O arquivo pode conter `api_key` literal, então em Unix é gravado com modo `0600`.
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        let content = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        #[cfg(unix)]
        {
            // `mode` só vale na criação; corrige também arquivos pré-existentes.
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        io::Write::write_all(&mut file, content.as_bytes())?;
        Ok(())
    }
}

impl Connection {
    /// Resolve a chave de API. Prefixo `env:NOME` lê a variável de ambiente;
    /// caso contrário, o valor é retornado literalmente.
    pub fn resolve_api_key(&self) -> Result<String, ConfigError> {
        match self.api_key.strip_prefix("env:") {
            Some(name) => env::var(name).map_err(|_| ConfigError::MissingEnv(name.to_string())),
            None => Ok(self.api_key.clone()),
        }
    }

    /// Chave utilizável (prontidão, spec 093 T12): [`Self::resolve_api_key`], a mesma resolução
    /// do cliente, sem erro e com valor não vazio. Chave vazia, `env:` com a variável ausente ou
    /// vazia e qualquer erro de resolução contam como inutilizáveis. Nunca expõe o valor.
    pub fn has_usable_key(&self) -> bool {
        self.resolve_api_key().is_ok_and(|key| !key.trim().is_empty())
    }

    /// Endereço utilizável: não vazio depois de tirar espaços e `/` finais. Cobre a recusa de
    /// `LlmClient::new` (`base_url` vazia após `trim_end_matches('/')`), um pouco mais estrito.
    pub fn has_usable_base_url(&self) -> bool {
        !self.base_url.trim().trim_end_matches('/').is_empty()
    }

    /// Problemas que impedem montar o cliente desta conexão (prontidão, spec 093 T12), na ordem
    /// endereço e chave. Lista vazia: `LlmClient::from_connection` não falha por configuração.
    pub fn usable_problems(&self) -> Vec<ConnectionProblem> {
        let mut out = Vec::new();
        if !self.has_usable_base_url() {
            out.push(ConnectionProblem::BaseUrl);
        }
        if !self.has_usable_key() {
            out.push(ConnectionProblem::ApiKey);
        }
        out
    }
}

/// Motivo de uma conexão inutilizável (sem detalhe: nunca carrega valor, variável nem URL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionProblem {
    /// `base_url` vazia.
    BaseUrl,
    /// Chave vazia ou não resolvível.
    ApiKey,
}

/// Caminho padrão: `$XDG_CONFIG_HOME/nants/config.toml` ou `$HOME/.config/nants/config.toml`.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    default_path_from(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

/// Versão pura de `default_path`, com as variáveis de ambiente injetadas.
/// `XDG_CONFIG_HOME` vazio ou relativo é ignorado (cai para `HOME/.config`).
fn default_path_from(
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let xdg = xdg_config_home
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty() && p.is_absolute());
    let base = match xdg {
        Some(dir) => dir,
        None => match home {
            Some(h) if !h.is_empty() => PathBuf::from(h).join(".config"),
            _ => return Err(ConfigError::HomeDirNotFound),
        },
    };
    Ok(base.join("nants").join("config.toml"))
}

/// Diretório de dados do Nants: `$XDG_DATA_HOME/nants` ou `$HOME/.local/share/nants`. `None` sem
/// nenhum dos dois (absolutos). Ponto único usado pelos overrides do Compose e pelo estado da UI.
pub fn data_dir() -> Option<PathBuf> {
    data_dir_from(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))
}

/// Versão pura de `data_dir`, com as variáveis de ambiente injetadas. Valor vazio ou relativo é
/// ignorado.
pub fn data_dir_from(xdg_data_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let abs = |v: OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    let base = xdg_data_home
        .and_then(abs)
        .or_else(|| home.and_then(abs).map(|h| h.join(".local").join("share")))?;
    Some(base.join("nants"))
}

/// Expande `~` (sozinho ou `~/...`) com o `$HOME` injetado. `None` quando o resultado não é
/// absoluto (`~` sem `$HOME` absoluto, `~usuario` ou caminho relativo).
pub fn expand_home_from(raw: &str, home: Option<OsString>) -> Option<PathBuf> {
    let home_dir = || home.clone().map(PathBuf::from).filter(|h| h.is_absolute());
    let path = if raw == "~" {
        home_dir()?
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home_dir()?.join(rest)
    } else {
        PathBuf::from(raw)
    };
    Some(path).filter(|p| p.is_absolute())
}

impl Config {
    /// Texto configurado da raiz da pasta de projetos (ou o padrão).
    pub fn projects_root_raw(&self) -> &str {
        self.projects_root.as_deref().unwrap_or(DEFAULT_PROJECTS_ROOT)
    }

    /// Raiz da pasta de projetos expandida (`~` → `$HOME`). `None` se não for absoluta.
    pub fn projects_root_path(&self) -> Option<PathBuf> {
        expand_home_from(self.projects_root_raw(), env::var_os("HOME"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    /// Diretório temporário único por teste.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("nants-config-{}-{}", process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    const EXAMPLE: &str = r#"
[server]
host = "127.0.0.1"
port = 3000

[[connections]]
id = "corp-gateway"
name = "Gateway Corporativo"
base_url = "https://corp-ai.example.com/v1"
api_key = "env:CORP_API_KEY"

[[connections]]
id = "openai-direct"
name = "OpenAI Cloud Direct"
base_url = "https://api.openai.com/v1"
api_key = "env:OPENAI_API_KEY"

[[connections]]
id = "router-gw"
name = "Roteador"
base_url = "https://router.example.com/api/v1"
api_key = "env:ROUTER_API_KEY"

[agents.architect]
connection_id = "corp-gateway"
model = "gpt-4o-global-std"
effort = "low"

[agents.developer]
connection_id = "openai-direct"
model = "o3-mini"
effort = "low"

[agents.reviewer]
connection_id = "router-gw"
model = "anthropic/claude-haiku-4.5"
effort = "medium"

[[projects]]
id = "proj-api-pagamentos"
name = "api-pagamentos"
path = "/home/user/dev/api-pagamentos"
created_at = "2026-10-13T21:00:00Z"
"#;

    #[test]
    fn legacy_provider_type_and_api_version_are_ignored_on_load_and_not_rewritten() {
        let dir = temp_dir("legacy");
        let path = dir.join("config.toml");
        let legacy = r#"
[[connections]]
id = "az"
name = "Antiga"
provider_type = "azure"
base_url = "https://corp.example.com/openai/v1"
api_key = "env:AZ_KEY"
api_version = "2024-10-21"

[[connections]]
id = "oa"
name = "OpenAI"
provider_type = "openai"
base_url = "https://api.openai.com/v1"
api_key = "env:OA_KEY"
"#;
        fs::create_dir_all(&dir).expect("dir");
        fs::write(&path, legacy).expect("write");
        let loaded = Config::load_from(&path).expect("config antiga deve carregar");
        assert_eq!(loaded.connections.len(), 2);
        assert_eq!(loaded.connections[0].base_url, "https://corp.example.com/openai/v1");
        assert_eq!(loaded.connections[1].api_key, "env:OA_KEY");

        loaded.save_to(&path).expect("save");
        let rewritten = fs::read_to_string(&path).expect("read");
        assert!(!rewritten.contains("provider_type"), "{rewritten}");
        assert!(!rewritten.contains("api_version"), "{rewritten}");
        assert_eq!(Config::load_from(&path).expect("reload"), loaded);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn example_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("config.toml");
        let parsed: Config = toml::from_str(EXAMPLE).expect("exemplo válido");
        assert_eq!(parsed.connections.len(), 3);
        assert_eq!(parsed.agents.reviewer.effort, Effort::Medium);
        assert_eq!(parsed.projects[0].name, "api-pagamentos");

        parsed.save_to(&path).expect("save");
        let reloaded = Config::load_from(&path).expect("load");
        assert_eq!(reloaded, parsed);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_returns_default_without_creating_it() {
        let dir = temp_dir("missing");
        let path = dir.join("config.toml");
        let cfg = Config::load_from(&path).expect("load");
        assert_eq!(cfg, Config::default());
        assert!(!path.exists());
        assert!(!dir.exists());
    }

    #[test]
    #[cfg(unix)]
    fn save_writes_file_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("mode");
        let path = dir.join("config.toml");
        Config::default().save_to(&path).expect("save");
        let mode = fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "modo obtido: {:o}", mode & 0o777);

        // Arquivo pré-existente com permissões amplas também deve ficar 0600.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
        Config::default().save_to(&path).expect("resave");
        let mode = fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "modo após regravar: {:o}", mode & 0o777);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expand_home_handles_tilde_and_rejects_relative() {
        let home = || Some(OsString::from("/home/u"));
        assert_eq!(expand_home_from("~", home()), Some(PathBuf::from("/home/u")));
        assert_eq!(expand_home_from("~/NantsProjects", home()), Some(PathBuf::from("/home/u/NantsProjects")));
        assert_eq!(expand_home_from("/srv/p", None), Some(PathBuf::from("/srv/p")));
        assert_eq!(expand_home_from("~/x", None), None);
        assert_eq!(expand_home_from("~/x", Some(OsString::from("rel"))), None);
        assert_eq!(expand_home_from("~outro/x", home()), None);
        assert_eq!(expand_home_from("rel/x", home()), None);
        assert_eq!(Config::default().projects_root_raw(), "~/NantsProjects");
    }

    #[test]
    fn data_dir_prefers_absolute_xdg_and_falls_back_to_home() {
        let xdg = |v: &str| Some(OsString::from(v));
        assert_eq!(data_dir_from(xdg("/x/data"), xdg("/home/u")), Some(PathBuf::from("/x/data/nants")));
        assert_eq!(data_dir_from(xdg("rel"), xdg("/home/u")), Some(PathBuf::from("/home/u/.local/share/nants")));
        assert_eq!(data_dir_from(xdg(""), xdg("/home/u")), Some(PathBuf::from("/home/u/.local/share/nants")));
        assert_eq!(data_dir_from(None, xdg("rel")), None);
        assert_eq!(data_dir_from(None, None), None);
    }

    #[test]
    fn default_path_uses_valid_absolute_xdg() {
        let p = default_path_from(
            Some(OsString::from("/xdg/cfg")),
            Some(OsString::from("/home/u")),
        )
        .expect("path");
        assert_eq!(p, PathBuf::from("/xdg/cfg/nants/config.toml"));
    }

    #[test]
    fn default_path_ignores_relative_xdg() {
        let p = default_path_from(
            Some(OsString::from("relativo/cfg")),
            Some(OsString::from("/home/u")),
        )
        .expect("path");
        assert_eq!(p, PathBuf::from("/home/u/.config/nants/config.toml"));
    }

    #[test]
    fn default_path_ignores_empty_xdg() {
        let p = default_path_from(Some(OsString::new()), Some(OsString::from("/home/u")))
            .expect("path");
        assert_eq!(p, PathBuf::from("/home/u/.config/nants/config.toml"));
    }

    #[test]
    fn default_path_falls_back_to_home_when_xdg_absent() {
        let p = default_path_from(None, Some(OsString::from("/home/u"))).expect("path");
        assert_eq!(p, PathBuf::from("/home/u/.config/nants/config.toml"));
    }

    #[test]
    fn default_path_errors_when_home_and_xdg_missing() {
        let err = default_path_from(None, None).expect_err("deve falhar");
        assert!(matches!(err, ConfigError::HomeDirNotFound), "obtido: {err:?}");
        let err = default_path_from(Some(OsString::from("rel")), Some(OsString::new()))
            .expect_err("deve falhar");
        assert!(matches!(err, ConfigError::HomeDirNotFound), "obtido: {err:?}");
    }

    #[test]
    fn invalid_toml_returns_parse_error() {
        let dir = temp_dir("invalid");
        fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("config.toml");
        fs::write(&path, "[server\nhost = ").expect("write");
        let err = Config::load_from(&path).expect_err("deve falhar");
        assert!(matches!(err, ConfigError::Parse(_)), "obtido: {err:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_parent_directories() {
        let dir = temp_dir("save");
        let path = dir.join("a").join("b").join("config.toml");
        Config::default().save_to(&path).expect("save");
        assert!(path.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    fn connection_with_key(api_key: &str) -> Connection {
        Connection {
            id: "c".into(),
            name: "c".into(),
            base_url: "http://localhost".into(),
            api_key: api_key.into(),
            wire_api: WireApi::default(),
        }
    }

    #[test]
    fn legacy_config_without_wire_api_loads_as_chat_and_rewrites_it() {
        let dir = temp_dir("wire-legacy");
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            &path,
            r#"
[[connections]]
id = "oa"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "env:OA_KEY"
"#,
        )
        .expect("write");
        let loaded = Config::load_from(&path).expect("carrega");
        assert_eq!(loaded.connections[0].wire_api, WireApi::Chat);
        loaded.save_to(&path).expect("save");
        let rewritten = fs::read_to_string(&path).expect("read");
        assert!(rewritten.contains("wire_api = \"chat\""), "{rewritten}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn wire_api_responses_round_trips_through_toml() {
        let dir = temp_dir("wire-roundtrip");
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.connections.push(Connection {
            wire_api: WireApi::Responses,
            ..connection_with_key("k")
        });
        cfg.save_to(&path).expect("save");
        let text = fs::read_to_string(&path).expect("read");
        assert!(text.contains("wire_api = \"responses\""), "{text}");
        assert_eq!(Config::load_from(&path).expect("load"), cfg);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_wire_api_is_static_load_error() {
        let dir = temp_dir("wire-invalid");
        let path = dir.join("config.toml");
        fs::create_dir_all(&dir).expect("dir");
        fs::write(
            &path,
            r#"
[[connections]]
id = "oa"
name = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "k"
wire_api = "foo"
"#,
        )
        .expect("write");
        let err = Config::load_from(&path).expect_err("deve falhar");
        assert!(matches!(err, ConfigError::Parse(_)), "obtido: {err:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_api_key_literal() {
        let conn = connection_with_key("sk-literal");
        assert_eq!(conn.resolve_api_key().expect("literal"), "sk-literal");
    }

    #[test]
    fn resolve_api_key_reads_env() {
        let var = "NANTS_T02_TEST_KEY_PRESENT";
        env::set_var(var, "valor-do-env");
        let conn = connection_with_key(&format!("env:{var}"));
        assert_eq!(conn.resolve_api_key().expect("env"), "valor-do-env");
        env::remove_var(var);
    }

    #[test]
    fn resolve_api_key_missing_env_is_typed_error() {
        let var = "NANTS_T02_TEST_KEY_ABSENT";
        env::remove_var(var);
        let conn = connection_with_key(&format!("env:{var}"));
        match conn.resolve_api_key() {
            Err(ConfigError::MissingEnv(name)) => assert_eq!(name, var),
            other => panic!("esperado MissingEnv, obtido {other:?}"),
        }
    }

    #[test]
    fn usable_key_requires_a_resolvable_non_empty_value() {
        assert!(connection_with_key("sk-literal").has_usable_key());
        assert!(!connection_with_key("").has_usable_key());
        assert!(!connection_with_key("   ").has_usable_key());
        let absent = "NANTS_TEST_T12_CFG_KEY_NEVER_SET";
        assert!(!connection_with_key(&format!("env:{absent}")).has_usable_key());
        let empty = "NANTS_TEST_T12_CFG_KEY_EMPTY";
        env::set_var(empty, "");
        assert!(!connection_with_key(&format!("env:{empty}")).has_usable_key());
        env::remove_var(empty);
        let present = "NANTS_TEST_T12_CFG_KEY_PRESENT";
        env::set_var(present, "valor");
        assert!(connection_with_key(&format!("env:{present}")).has_usable_key());
        env::remove_var(present);
    }

    #[test]
    fn usable_problems_cover_base_url_and_key() {
        assert!(connection_with_key("sk-literal").usable_problems().is_empty());
        let mut conn = connection_with_key("");
        assert_eq!(conn.usable_problems(), [ConnectionProblem::ApiKey]);
        for url in ["", "  ", "/", " // "] {
            conn.base_url = url.into();
            assert_eq!(conn.usable_problems(), [ConnectionProblem::BaseUrl, ConnectionProblem::ApiKey], "{url:?}");
        }
        conn.api_key = "sk-literal".into();
        assert_eq!(conn.usable_problems(), [ConnectionProblem::BaseUrl]);
    }
}
