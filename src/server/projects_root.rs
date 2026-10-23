//! Pasta de projetos gerenciada (spec 093 T02, decisão 16).
//!
//! - A raiz vem de `projects_root` na config (padrão `~/NantsProjects`). Validação: caminho absoluto
//!   depois de expandir `~`, sem `..`, fora dos diretórios de sistema e fora de projeto registrado.
//! - "Novo projeto" por nome: o slug (minúsculas, `[a-z0-9-]`, até 64 caracteres) só nomeia a pasta;
//!   `<raiz>/<slug>` é criada com `create_dir` (que nunca segue symlink: um link no lugar conta como
//!   "já existe") e conferida dentro da raiz canônica.
//!
//! Mensagens estáticas: o caminho só sai na resposta do 409 `project_dir_exists`, que é o caminho
//! que o próprio Nants montou.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map};

use super::error::ApiError;
use super::projects::{is_system_dir, MAX_PATH_BYTES};
use crate::config::Config;

/// Tamanho máximo do slug da pasta.
pub const FOLDER_SLUG_MAX: usize = 64;
/// Código do 409 de pasta existente.
pub const PROJECT_DIR_EXISTS: &str = "project_dir_exists";

const ROOT_CREATE: &str = "projects_root_create_failed";
const DIR_CREATE: &str = "project_dir_create_failed";
const DIR_OUTSIDE: &str = "project_dir_outside_root";

/// Letra latina acentuada → base ASCII (o resto passa como está).
fn fold(ch: char) -> char {
    match ch {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ç' => 'c',
        'ñ' => 'n',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

/// Slug da pasta: minúsculas, `[a-z0-9-]`, sem hífens repetidos nem nas pontas, até 64 caracteres.
/// `None` quando nada sobra (nome vazio ou só símbolos). Separadores e `..` nunca sobrevivem.
pub fn folder_slug(name: &str) -> Option<String> {
    let mut out = String::new();
    for ch in name.chars().flat_map(char::to_lowercase).map(fold) {
        if out.len() >= FOLDER_SLUG_MAX {
            break;
        }
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let slug = out.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Valida o texto de `projects_root` e devolve o caminho expandido (não exige existir).
pub fn validate_root(raw: &str, cfg: &Config, home: Option<std::ffi::OsString>) -> Result<PathBuf, ApiError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "projects_root_absolute", Map::new()));
    }
    let path = crate::config::expand_home_from(raw, home).ok_or_else(|| ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "projects_root_absolute", Map::new()))?;
    if path.components().any(|c| c == Component::ParentDir) || path.parent().is_none() {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "projects_root_absolute", Map::new()));
    }
    check_placement(&path, cfg)?;
    // Raiz já existente: a forma canônica (symlinks resolvidos) também passa pelas regras.
    if let Ok(canonical) = fs::canonicalize(&path) {
        if canonical.parent().is_none() {
            return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "projects_root_absolute", Map::new()));
        }
        check_placement(&canonical, cfg)?;
    }
    Ok(path)
}

fn check_placement(path: &Path, cfg: &Config) -> Result<(), ApiError> {
    if is_system_dir(path) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "projects_root_system", Map::new()));
    }
    if cfg.projects.iter().any(|p| path.starts_with(&p.path)) {
        return Err(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, "projects_root_inside_project", Map::new()));
    }
    Ok(())
}

/// Resultado da criação de `<raiz>/<slug>`.
#[derive(Debug)]
pub enum CreateDirError {
    /// Já existe algo no caminho (pasta, arquivo ou symlink).
    Exists(PathBuf),
    /// Recusa com mensagem estática.
    Api(ApiError),
}

impl IntoResponse for CreateDirError {
    fn into_response(self) -> Response {
        match self {
            Self::Exists(path) => ApiError::coded(StatusCode::CONFLICT, PROJECT_DIR_EXISTS, Map::new())
                .with_field("path", json!(path.display().to_string()))
                .into_response(),
            Self::Api(e) => e.into_response(),
        }
    }
}

/// Erro 400 de nome sem slug.
pub fn name_invalid() -> ApiError {
    ApiError::coded(StatusCode::BAD_REQUEST, "project_name_invalid", Map::new())
}

/// Cria a raiz se faltar e `<raiz>/<slug>` (bloqueante). Devolve o caminho canônico da pasta nova.
pub fn create_project_dir(root: &Path, slug: &str, cfg: &Config) -> Result<PathBuf, CreateDirError> {
    let api = |code: &'static str| CreateDirError::Api(ApiError::coded(StatusCode::UNPROCESSABLE_ENTITY, code, Map::new()));
    fs::create_dir_all(root).map_err(|_| api(ROOT_CREATE))?;
    let canonical_root = fs::canonicalize(root).map_err(|_| api(ROOT_CREATE))?;
    if !canonical_root.is_dir() {
        return Err(api(ROOT_CREATE));
    }
    check_placement(&canonical_root, cfg).map_err(CreateDirError::Api)?;
    let target = canonical_root.join(slug);
    // `slug` é `[a-z0-9-]`: um único componente normal, sempre filho direto da raiz.
    if target.parent() != Some(canonical_root.as_path()) {
        return Err(api(DIR_OUTSIDE));
    }
    match fs::create_dir(&target) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Err(CreateDirError::Exists(target)),
        Err(_) => return Err(api(DIR_CREATE)),
    }
    // A pasta recém-criada não pode ser link e precisa continuar dentro da raiz.
    let meta = fs::symlink_metadata(&target).map_err(|_| api(DIR_CREATE))?;
    let canonical = fs::canonicalize(&target).map_err(|_| api(DIR_CREATE))?;
    if !meta.is_dir() || canonical.parent() != Some(canonical_root.as_path()) {
        return Err(api(DIR_OUTSIDE));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn slug_rules() {
        assert_eq!(folder_slug("Órbita Kids").as_deref(), Some("orbita-kids"));
        assert_eq!(folder_slug("  Meu   Projeto!! Ç ").as_deref(), Some("meu-projeto-c"));
        assert_eq!(folder_slug("../../etc/passwd").as_deref(), Some("etc-passwd"));
        assert_eq!(folder_slug("a/b\\c").as_deref(), Some("a-b-c"));
        assert_eq!(folder_slug("app_v2.0").as_deref(), Some("app-v2-0"));
        assert_eq!(folder_slug(""), None);
        assert_eq!(folder_slug("   "), None);
        assert_eq!(folder_slug("!!! ... ///"), None);
        assert_eq!(folder_slug("日本"), None);
        let long = folder_slug(&"ab-".repeat(60)).expect("slug");
        assert!(long.len() <= FOLDER_SLUG_MAX && !long.ends_with('-'), "{long}");
        let s = folder_slug(&"x".repeat(200)).expect("slug");
        assert_eq!(s.len(), FOLDER_SLUG_MAX);
    }

    #[test]
    fn root_validation() {
        let home = || Some(OsString::from("/home/u"));
        let mut cfg = Config::default();
        assert_eq!(validate_root("~/NantsProjects", &cfg, home()).expect("ok"), PathBuf::from("/home/u/NantsProjects"));
        for bad in ["", "rel/x", "~/a/../b", "/", "/etc/nants", "/proc/x", "~x/y", "/a\nb"] {
            assert!(validate_root(bad, &cfg, home()).is_err(), "{bad:?}");
        }
        assert!(validate_root("~/x", &cfg, None).is_err(), "~ sem HOME");
        cfg.projects.push(crate::config::Project {
            id: "p".into(),
            name: "p".into(),
            path: "/home/u/proj".into(),
            created_at: "x".into(),
        });
        assert!(validate_root("/home/u/proj/sub", &cfg, home()).is_err());
        assert!(validate_root("/home/u/proj", &cfg, home()).is_err());
        assert!(validate_root("/home/u/projetos", &cfg, home()).is_ok(), "prefixo de texto não é ancestral");
    }
}

#[cfg(test)]
#[path = "projects_root_tests.rs"]
mod route_tests;
