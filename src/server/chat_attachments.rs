//! Imagens anexadas pelo usuário na conversa com o Arquiteto (spec 082/T05).
//!
//! - `POST /api/projects/:id/chat/attachments`: corpo = bytes da imagem, `Content-Type`
//!   `image/png`, `image/jpeg` ou `image/webp`, conferido pela assinatura do conteúdo (415 se não
//!   confere). Até [`MAX_ATTACHMENT_BYTES`] (413, limite de corpo próprio da rota). Grava em
//!   `.sdd/specs/.chat-attachments/<id>.<ext>` (id hex aleatório de 32 caracteres) sem seguir
//!   symlinks; o diretório fica fora do Git pelo exclude local. Teto de [`MAX_PROJECT_BYTES`] por
//!   projeto: acima dele apaga os anexos mais antigos que não aparecem nos últimos
//!   [`KEEP_TURNS`] turnos; se ainda não couber, 507. Antes da cota, cada upload apaga os anexos
//!   órfãos (spec 085 T02, [`sweep_orphans`]): mais velhos que [`ORPHAN_TTL`], fora de todos os
//!   turnos da conversa e fora do `references` de todas as specs.
//! - `GET /api/projects/:id/chat/attachments/:name`: mesmas regras do endpoint de capturas
//!   (nome validado, `safe_fs::read_bounded`, `nosniff`, `Cache-Control` privado; 404 estático).

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map};

use super::approvals::random_hex_id;
use super::error::ApiError;
use super::events::validate_project_id;
use super::projects::{ensure_local_excludes, find_project};
use super::safe_fs::{self, SafeFsError, O_NOFOLLOW};
use super::state::AppState;
use crate::agents::llm_client::ImageInput;

/// Diretório dos anexos, relativo à raiz do projeto.
pub const ATTACHMENTS_REL: &str = crate::sdd::references::CHAT_ATTACHMENTS_REL;
/// Linha do exclude local que tira os anexos do Git.
pub const ATTACHMENTS_EXCLUDE_LINE: &str = ".sdd/specs/.chat-attachments/";
/// Tamanho máximo de uma imagem (5 MiB): também o limite de corpo da rota de upload.
pub const MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
/// Teto de espaço dos anexos por projeto (200 MiB).
pub const MAX_PROJECT_BYTES: u64 = 200 * 1024 * 1024;
/// Anexos citados nestes últimos turnos nunca são apagados para abrir espaço.
pub const KEEP_TURNS: usize = 50;
/// Idade (mtime) a partir da qual um anexo não citado em lugar nenhum é apagado no upload.
pub const ORPHAN_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Anexos por mensagem.
pub const MAX_MESSAGE_ATTACHMENTS: usize = 4;

const CACHE_CONTROL: &str = "private, max-age=3600";

/// Serializa a gravação (cota e limpeza) entre uploads simultâneos.
static STORE_GUARD: Mutex<()> = Mutex::new(());

/// Tipos aceitos: (MIME, extensão).
const KINDS: [(&str, &str); 3] = [("image/png", "png"), ("image/jpeg", "jpg"), ("image/webp", "webp")];

/// MIME pela assinatura do conteúdo: PNG, JPEG ou WebP (regra única em [`crate::sdd::references`]).
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    crate::sdd::references::sniff(bytes)
}

/// Nome de anexo aceito: 32 hex minúsculos + `.png`, `.jpg`, `.jpeg` ou `.webp` (regra única em
/// [`crate::sdd::references::attachment_mime`], a mesma de `references` na spec). Devolve o MIME.
pub fn attachment_mime(name: &str) -> Option<&'static str> {
    crate::sdd::references::attachment_mime(name)
}

fn rel_of(name: &str) -> String {
    format!("{ATTACHMENTS_REL}/{name}")
}

/// Imagem anexada `name` como entrada do modelo. `None` se o nome é inválido, o arquivo não existe,
/// é inseguro (symlink, especial), grande demais ou o conteúdo não confere com a extensão.
pub fn load_attachment(root: &Path, name: &str) -> Option<ImageInput> {
    let mime = attachment_mime(name)?;
    let read = safe_fs::read_bounded(root, &rel_of(name), MAX_ATTACHMENT_BYTES as u64).ok()?;
    let data = read.data?;
    (sniff(&data) == Some(mime)).then(|| ImageInput::new(mime, data))
}

/// O anexo existe como arquivo regular, sem symlink no caminho.
pub fn attachment_exists(root: &Path, name: &str) -> bool {
    if attachment_mime(name).is_none() {
        return false;
    }
    safe_fs::check_chain(root, &rel_of(name))
        .ok()
        .and_then(|path| fs::symlink_metadata(path).ok())
        .is_some_and(|m| m.file_type().is_file())
}

fn not_found() -> ApiError {
    ApiError::coded(StatusCode::NOT_FOUND, "image_not_found", Map::new())
}

fn unsupported() -> ApiError {
    ApiError::coded(StatusCode::UNSUPPORTED_MEDIA_TYPE, "image_type_unsupported", Map::new())
}

/// MIME declarado no `Content-Type` (parâmetros ignorados), se for um dos aceitos.
fn declared_mime(headers: &HeaderMap) -> Option<&'static str> {
    let value = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    let media = value.split(';').next()?.trim().to_ascii_lowercase();
    KINDS.iter().find(|(mime, _)| *mime == media).map(|(mime, _)| *mime)
}

/// `true` para `POST /api/projects/<id>/chat/attachments`: a única rota mutante com corpo de imagem.
pub fn is_upload_path(path: &str) -> bool {
    path.strip_prefix("/api/projects/")
        .and_then(|rest| rest.strip_suffix("/chat/attachments"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// MIME de imagem aceito no upload (para o middleware de `Content-Type`).
pub fn is_upload_content_type(headers: &HeaderMap) -> bool {
    declared_mime(headers).is_some()
}

/// Falha ao gravar um anexo.
#[derive(Debug, PartialEq, Eq)]
enum StoreError {
    Fs(SafeFsError),
    Full,
}

/// Arquivos regulares do diretório de anexos: (nome, tamanho, mtime). Symlinks e outros tipos são
/// ignorados (nunca seguidos nem contados).
fn list_files(dir: &Path) -> Vec<(String, u64, SystemTime)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            if !meta.file_type().is_file() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            Some((name, meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
        })
        .collect()
}

/// Abre espaço para `incoming` bytes dentro de `cap`: apaga os anexos mais antigos fora de `keep`.
fn make_room(root: &Path, dir: &Path, incoming: u64, cap: u64, keep: &HashSet<String>) -> Result<(), StoreError> {
    let mut files = list_files(dir);
    let mut total: u64 = files.iter().map(|(_, size, _)| size).sum();
    if total + incoming <= cap {
        return Ok(());
    }
    files.sort_by(|a, b| a.2.cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
    for (name, size, _) in files {
        if total + incoming <= cap {
            break;
        }
        if keep.contains(&name) {
            continue;
        }
        if safe_fs::remove_regular(root, &rel_of(&name)).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    if total + incoming <= cap {
        Ok(())
    } else {
        Err(StoreError::Full)
    }
}

/// Apaga os anexos órfãos (spec 085 T02) e devolve quantos apagou. Órfão = arquivo regular com nome
/// de anexo válido, mtime mais velho que `ttl` em relação a `now`, não citado em nenhum turno da
/// conversa e fora do `references` de todas as specs. Melhor esforço: diretório inseguro, conversa
/// ilegível ou falha ao apagar não apagam nada a mais e não viram erro. Symlinks nunca são seguidos.
fn sweep_orphans(root: &Path, now: SystemTime, ttl: Duration) -> usize {
    let Ok(dir) = safe_fs::check_chain(root, ATTACHMENTS_REL) else {
        return 0;
    };
    let old: Vec<String> = list_files(&dir)
        .into_iter()
        .filter(|(name, _, mtime)| {
            // mtime no futuro (relógio ajustado) conta como recente.
            attachment_mime(name).is_some() && now.duration_since(*mtime).is_ok_and(|age| age > ttl)
        })
        .map(|(name, _, _)| name)
        .collect();
    if old.is_empty() {
        return 0;
    }
    // Sem ler a conversa inteira não há como saber o que é órfão: não apaga nada.
    let Some(cited) = super::chat::all_attachments(root) else {
        return 0;
    };
    let referenced = crate::sdd::references::spec_references(root);
    old.iter()
        .filter(|name| !cited.contains(*name) && !referenced.contains(*name))
        .filter(|name| safe_fs::remove_regular(root, &rel_of(name)).is_ok())
        .count()
}

/// Grava `data` como novo anexo (`<id>.<ext>`), respeitando a cota `cap`. Devolve o nome.
fn store(root: &Path, data: &[u8], ext: &str, cap: u64) -> Result<String, StoreError> {
    let _guard = STORE_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    safe_fs::ensure_dirs(root, ATTACHMENTS_REL).map_err(StoreError::Fs)?;
    let dir = safe_fs::check_chain(root, ATTACHMENTS_REL).map_err(StoreError::Fs)?;
    // Melhor esforço: o resultado da limpeza de órfãos não decide o upload.
    sweep_orphans(root, SystemTime::now(), ORPHAN_TTL);
    let keep = super::chat::recent_attachments(root, KEEP_TURNS);
    make_room(root, &dir, data.len() as u64, cap, &keep)?;
    let id = random_hex_id().map_err(|_| StoreError::Fs(SafeFsError::Io))?;
    let name = format!("{id}.{ext}");
    let rel = rel_of(&name);
    let path = safe_fs::check_chain(root, &rel).map_err(StoreError::Fs)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&path)
        .map_err(|_| StoreError::Fs(SafeFsError::Io))?;
    if file.write_all(data).is_err() {
        drop(file);
        let _ = safe_fs::remove_regular(root, &rel);
        return Err(StoreError::Fs(SafeFsError::Io));
    }
    Ok(name)
}

/// `POST /api/projects/:id/chat/attachments`.
pub async fn post_attachment(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let declared = declared_mime(&headers).ok_or_else(unsupported)?;
    if body.len() > MAX_ATTACHMENT_BYTES {
        return Err(ApiError::coded(StatusCode::PAYLOAD_TOO_LARGE, "image_too_large", Map::new()));
    }
    if sniff(&body) != Some(declared) {
        return Err(unsupported());
    }
    let ext = KINDS.iter().find(|(mime, _)| *mime == declared).map_or("png", |(_, ext)| *ext);
    let project = find_project(&state, &id).await?;
    let root = PathBuf::from(&project.path);
    let stored = tokio::task::spawn_blocking(move || {
        // Melhor esforço: sem `.git` (ou exclude inseguro) o upload segue.
        let _ = ensure_local_excludes(&root);
        store(&root, &body, ext, MAX_PROJECT_BYTES)
    })
    .await
    .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?;
    match stored {
        Ok(name) => {
            let id = name.split_once('.').map_or(name.as_str(), |(id, _)| id).to_string();
            Ok((StatusCode::CREATED, Json(json!({ "id": id, "name": name }))).into_response())
        }
        Err(StoreError::Full) => Err(ApiError::coded(StatusCode::INSUFFICIENT_STORAGE, "attachment_no_space", Map::new())),
        Err(StoreError::Fs(SafeFsError::Unsafe | SafeFsError::NotRegular)) => {
            Err(ApiError::coded(StatusCode::CONFLICT, SafeFsError::Unsafe.code(), Map::new()))
        }
        Err(StoreError::Fs(_)) => Err(ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new())),
    }
}

/// `GET /api/projects/:id/chat/attachments/:name`.
pub async fn get_attachment(
    State(state): State<AppState>,
    AxPath((id, name)): AxPath<(String, String)>,
) -> Result<Response, ApiError> {
    validate_project_id(&id)?;
    let mime = attachment_mime(&name).ok_or_else(not_found)?;
    let project = find_project(&state, &id).await?;
    let root = PathBuf::from(&project.path);
    let rel = rel_of(&name);
    let read = tokio::task::spawn_blocking(move || safe_fs::read_bounded(&root, &rel, MAX_ATTACHMENT_BYTES as u64))
        .await
        .map_err(|_| ApiError::coded(StatusCode::INTERNAL_SERVER_ERROR, "internal", Map::new()))?;
    let data = match read {
        Ok(safe_fs::BoundedRead { data: Some(data), .. }) => data,
        _ => return Err(not_found()),
    };
    let mut resp = data.into_response();
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(resp)
}

#[cfg(test)]
#[path = "chat_attachments_tests.rs"]
mod tests;

/// Limpeza de anexos órfãos (spec 085 T02), sem servidor: "agora" e TTL injetados.
#[cfg(test)]
mod sweep_tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use super::{store, sweep_orphans, ATTACHMENTS_REL, KEEP_TURNS, MAX_PROJECT_BYTES, ORPHAN_TTL};
    use crate::agents::architect::{ChatTurn, TurnRole, CHAT_FILE_REL};
    use crate::server::chat::append_turns;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nresto-da-imagem";
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    fn repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nants-sweep-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let root = dir.join("repo");
        fs::create_dir_all(root.join(ATTACHMENTS_REL)).expect("mkdir");
        root
    }

    fn name(i: u32) -> String {
        format!("{i:032x}.png")
    }

    fn age_file(path: &Path, now: SystemTime, age: Duration) {
        let file = fs::File::options().write(true).open(path).expect("abrir");
        file.set_modified(now - age).expect("mtime");
    }

    /// Cria o anexo `name` com mtime `age` antes de `now`.
    fn attach(root: &Path, name: &str, now: SystemTime, age: Duration) -> PathBuf {
        let path = root.join(ATTACHMENTS_REL).join(name);
        fs::write(&path, PNG).expect("arquivo");
        age_file(&path, now, age);
        path
    }

    fn turn(attachments: &[&str]) -> ChatTurn {
        ChatTurn {
            role: TurnRole::User,
            text: "veja".into(),
            ts: 1,
            turn_id: None,
            internal: false,
            attachments: attachments.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `spec.md` mínimo com `references` em `<base>/.sdd/specs/<id>/`. Devolve o diretório.
    fn spec_with_refs(base: &Path, id: &str, refs: &[&str]) -> PathBuf {
        let dir = base.join(".sdd/specs").join(id);
        fs::create_dir_all(&dir).expect("spec dir");
        let list: Vec<String> = refs.iter().map(|r| format!("\"{r}\"")).collect();
        let text = format!(
            "---\nid: \"{id}\"\ntitle: \"t\"\nstatus: \"DRAFT\"\nbase_branch: \"main\"\ncreated_at: \"2026-10-18\"\nreferences: [{}]\n---\ncorpo\n",
            list.join(", ")
        );
        fs::write(dir.join("spec.md"), text).expect("spec.md");
        dir
    }

    #[test]
    fn old_orphan_goes_recent_orphan_stays() {
        let root = repo("age");
        let now = SystemTime::now();
        let old = attach(&root, &name(1), now, DAY + Duration::from_secs(60));
        let recent = attach(&root, &name(2), now, DAY - Duration::from_secs(60));
        // Nome fora do formato de anexo nunca é tocado.
        let other = root.join(ATTACHMENTS_REL).join("nota.png");
        fs::write(&other, PNG).expect("outro");
        age_file(&other, now, DAY * 3);

        assert_eq!(sweep_orphans(&root, now, ORPHAN_TTL), 1);
        assert!(!old.exists());
        assert!(recent.exists());
        assert!(other.exists());
    }

    #[test]
    fn attachment_cited_in_an_old_turn_stays() {
        let root = repo("cited");
        let now = SystemTime::now();
        let cited = attach(&root, &name(1), now, DAY * 10);
        let orphan = attach(&root, &name(2), now, DAY * 10);
        assert!(append_turns(&root, &[turn(&[&name(1)])]));
        // Muitos turnos depois: fora da janela da cota, mas ainda citado na conversa.
        let filler: Vec<ChatTurn> = (0..KEEP_TURNS * 2).map(|_| turn(&[])).collect();
        assert!(append_turns(&root, &filler));

        assert_eq!(sweep_orphans(&root, now, ORPHAN_TTL), 1);
        assert!(cited.exists());
        assert!(!orphan.exists());
    }

    #[test]
    fn attachment_in_spec_references_stays_and_bad_specs_are_ignored() {
        let root = repo("refs");
        let now = SystemTime::now();
        let referenced = attach(&root, &name(1), now, DAY * 10);
        let orphan = attach(&root, &name(2), now, DAY * 10);
        spec_with_refs(&root, "001-a", &[&name(1)]);
        // Spec ilegível: ignorada sem falhar.
        let bad = root.join(".sdd/specs/002-b");
        fs::create_dir_all(&bad).expect("bad");
        fs::write(bad.join("spec.md"), "sem frontmatter").expect("w");

        assert_eq!(sweep_orphans(&root, now, ORPHAN_TTL), 1);
        assert!(referenced.exists());
        assert!(!orphan.exists());
    }

    #[test]
    fn symlinks_are_not_followed_nor_removed() {
        let root = repo("links");
        let now = SystemTime::now();
        // Alvo fora do diretório de anexos, antigo e com nome de anexo.
        let outside = root.parent().expect("pai").join("fora");
        fs::create_dir_all(&outside).expect("fora");
        let target = outside.join(name(9));
        fs::write(&target, PNG).expect("alvo");
        age_file(&target, now, DAY * 10);
        let link = root.join(ATTACHMENTS_REL).join(name(1));
        symlink(&target, &link).expect("symlink");
        // Diretório de spec que é symlink: ignorado, suas referências não protegem nada.
        let linked_spec = spec_with_refs(&outside, "x", &[&name(2)]);
        symlink(&linked_spec, root.join(".sdd/specs/003-link")).expect("symlink spec");
        let orphan = attach(&root, &name(2), now, DAY * 10);

        assert_eq!(sweep_orphans(&root, now, ORPHAN_TTL), 1);
        assert!(fs::symlink_metadata(&link).is_ok(), "symlink fica");
        assert!(target.exists(), "alvo intocado");
        assert!(!orphan.exists(), "spec via symlink não protege");
    }

    #[test]
    fn upload_works_when_the_chat_is_unreadable() {
        let root = repo("unreadable");
        let now = SystemTime::now();
        let old = attach(&root, &name(1), now, DAY * 10);
        // Conversa como symlink: ilegível com segurança, a varredura não apaga nada.
        let outside = root.parent().expect("pai").join("chat.jsonl");
        fs::write(&outside, "").expect("w");
        symlink(&outside, root.join(CHAT_FILE_REL)).expect("symlink");

        assert_eq!(sweep_orphans(&root, now, ORPHAN_TTL), 0);
        let stored = store(&root, PNG, "png", MAX_PROJECT_BYTES).expect("upload segue");
        assert!(root.join(ATTACHMENTS_REL).join(stored).exists());
        assert!(old.exists(), "sem ler a conversa, nada é órfão");
    }

    #[test]
    fn store_sweeps_old_orphans() {
        let root = repo("store");
        let now = SystemTime::now();
        let old = attach(&root, &name(1), now, DAY * 2);
        let stored = store(&root, PNG, "png", MAX_PROJECT_BYTES).expect("upload");
        assert!(!old.exists());
        assert!(root.join(ATTACHMENTS_REL).join(stored).exists());
    }
}
