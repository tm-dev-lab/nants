//! Atualização do `ARCHITECTURE.md` ao concluir uma spec (depois do relatório final).
//!
//! Um turno somente leitura do Arquiteto devolve o NOVO conteúdo completo do arquivo. O servidor
//! valida (Markdown, ≤ 64 KiB, seções fixas), grava de forma atômica e commita só esse arquivo
//! (`docs(<spec>): atualiza ARCHITECTURE.md`), com a base limpa. Qualquer falha vira aviso no log
//! e nada é gravado; nunca bloqueia a conclusão da spec.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::watch;

use super::approvals::ProjectApprover;
use super::chat::{architect_client, load_policy};
use super::events::{BusActivitySink, SecretSet};
use super::projects::scrub_text;
use super::report::{build_facts, wait_shutdown};
use super::safe_fs::{self, O_NOFOLLOW};
use super::spec_result::{has_real_git_dir, validate_spec_id};
use super::state::AppState;
use crate::agents::arch_memory::{localize_headings, sections_in_order, ARCH_FILE, ARCH_MAX_FILE_BYTES, ARCH_SECTION_KEYS};
use crate::agents::architect::LlmArchitect;
use crate::agents::chat::ChatBackend;
use crate::harness::git::Git;

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Desfecho de uma tentativa de atualização.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Arquivo gravado e commitado.
    Committed,
    /// Conteúdo idêntico ao atual: nada a fazer.
    Unchanged,
}

/// Motivo (código fixo) de a atualização não ter sido aplicada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip(pub &'static str);

/// Roda a atualização; falhas viram aviso no log (`eprintln`), nunca erro.
pub async fn update_after_spec(
    state: &AppState,
    project_id: &str,
    spec_id: &str,
    root: PathBuf,
    mut shutdown: watch::Receiver<bool>,
) {
    if validate_spec_id(spec_id).is_err() || !has_real_git_dir(&root) {
        return;
    }
    match run(state, project_id, spec_id, &root, &mut shutdown).await {
        Ok(_) => {}
        Err(Skip(code)) => {
            eprintln!("aviso: ARCHITECTURE.md não atualizado ({spec_id}): {code}");
        }
    }
}

async fn run(
    state: &AppState,
    project_id: &str,
    spec_id: &str,
    root: &Path,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Outcome, Skip> {
    // Base limpa exigida antes de gastar um turno do modelo.
    let git_root = root.to_path_buf();
    let clean = tokio::task::spawn_blocking(move || Git::new(git_root).is_clean_ignoring_sdd().unwrap_or(false))
        .await
        .unwrap_or(false);
    if !clean {
        return Err(Skip("base_suja"));
    }
    let secrets = state.secret_set().await;
    let current = match fs::symlink_metadata(root.join(ARCH_FILE)) {
        Err(_) => String::new(),
        Ok(_) => crate::agents::arch_memory::read_raw(root).ok_or(Skip("arquivo_inseguro"))?,
    };
    let sid = spec_id.to_string();
    let facts_root = root.to_path_buf();
    let facts_secrets = Arc::clone(&secrets);
    let facts = tokio::task::spawn_blocking(move || build_facts(&facts_root, &sid, &facts_secrets))
        .await
        .ok()
        .flatten()
        .ok_or(Skip("spec_ilegivel"))?;
    let current_block = if current.trim().is_empty() {
        "(o arquivo ainda não existe: crie-o seguindo o template)".to_string()
    } else {
        scrub_text(&secrets, &current)
    };
    let lessons_now = current_lessons_block(root, &secrets);
    let facts = format!("{facts}{lessons_now}\nARCHITECTURE.md ATUAL:\n{current_block}\n");

    let (model, effort, client) = architect_client(state).await.map_err(|_| Skip("nao_configurado"))?;
    let backend: Arc<dyn ChatBackend> = match state.report_backend() {
        Some(backend) => backend,
        None => Arc::new(client),
    };
    let policy = load_policy(root.to_path_buf()).await;
    let approver = Arc::new(ProjectApprover::new(
        project_id,
        root.to_path_buf(),
        state.approvals().clone(),
        state.events().clone(),
        Arc::clone(&secrets),
    )
    .with_lang(state.project_lang(project_id)));
    let activity = Arc::new(BusActivitySink::new(
        state.events().clone(),
        project_id,
        Arc::clone(&secrets),
    ).with_stage("architecture", "Atualizando ARCHITECTURE.md"));
    let architect = LlmArchitect::new(backend, model, Some(effort), policy, approver)
        .with_lang(state.project_lang(project_id))
        .with_activity(activity, Some(spec_id.to_string()), None)
        .with_usage(super::usage::FileUsageSink::redacting(root.to_path_buf(), project_id, Arc::clone(&secrets)));
    let turn = architect.update_architecture(root, &facts);
    let answer = tokio::select! {
        biased;
        _ = wait_shutdown(shutdown) => return Err(Skip("shutdown")),
        joined = tokio::time::timeout(state.report_timeout(), turn) => match joined {
            Err(_) => return Err(Skip("timeout")),
            Ok(Err(_)) => return Err(Skip("falha_do_provedor")),
            Ok(Ok(text)) => text,
        },
    };
    let sid = spec_id.to_string();
    let apply_root = root.to_path_buf();
    let lang = state.project_lang(project_id).get();
    tokio::task::spawn_blocking(move || {
        let (arch_answer, lessons) = split_lessons(&answer);
        let outcome = apply(lang, &apply_root, &sid, &arch_answer, &secrets);
        // Lições dos perfis (spec 063): independentes do ARCHITECTURE.md; falha só vira aviso.
        if let Err(Skip(code)) = apply_lessons(lang, &apply_root, &sid, &lessons, &secrets) {
            eprintln!("aviso: lições dos perfis não atualizadas ({sid}): {code}");
        }
        outcome
    })
    .await
    .map_err(|_| Skip("interno"))?
}

/// Marca de início do bloco de lições de um papel, após o ARCHITECTURE.md na resposta.
const LESSONS_MARK: &str = "=== LICOES ";

/// Separa o ARCHITECTURE.md das lições opcionais (`=== LICOES <papel> ===` + itens `- ...`).
/// Sem blocos, devolve a resposta inteira e nenhuma lição.
pub(super) fn split_lessons(answer: &str) -> (String, Vec<(String, Vec<String>)>) {
    let mut arch = String::new();
    let mut blocks: Vec<(String, Vec<String>)> = Vec::new();
    for line in answer.lines() {
        if let Some(rest) = line.trim().strip_prefix(LESSONS_MARK) {
            let role = rest.trim_end_matches('=').trim().to_lowercase();
            blocks.push((role, Vec::new()));
        } else if let Some((_, items)) = blocks.last_mut() {
            items.push(line.to_string());
        } else {
            arch.push_str(line);
            arch.push('\n');
        }
    }
    (arch, blocks)
}

/// Bloco de fatos com as lições atuais de cada perfil (o modelo as mescla com as novas).
fn current_lessons_block(root: &Path, secrets: &SecretSet) -> String {
    use crate::sdd::skills::{read_profile, section, LESSONS_SECTION, ROLES};
    let mut out = String::new();
    for role in ROLES {
        let Some(profile) = read_profile(root, role) else { continue };
        let lessons = section(&profile.body, LESSONS_SECTION).unwrap_or_default();
        let lessons = if lessons.is_empty() { "(nenhuma)".to_string() } else { lessons };
        out.push_str(&format!("LIÇÕES ATUAIS DO PERFIL {role}:\n{}\n", scrub_text(secrets, &lessons)));
    }
    out
}

/// Aplica as lições aos perfis existentes: só a seção `## Lições do projeto` muda (até 5 itens,
/// cada um com origem Txx); o resto do perfil, inclusive as notas do usuário, fica intacto.
/// Commita só os perfis alterados; falha de gravação/commit restaura os originais.
pub(super) fn apply_lessons(
    lang: crate::i18n::Lang,
    root: &Path,
    spec_id: &str,
    blocks: &[(String, Vec<String>)],
    secrets: &SecretSet,
) -> Result<usize, Skip> {
    use crate::sdd::skills::{clean_lessons, rel_path, validate_with, with_lessons, MAX_LESSONS_SECTION_BYTES, MAX_PROFILE_BYTES, ROLES};
    if blocks.is_empty() {
        return Ok(0);
    }
    let today = super::projects::format_utc(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    );
    let today = today.get(..10).unwrap_or("").to_string();
    let mut changed: Vec<(String, String, String)> = Vec::new(); // (rel, antigo, novo)
    for role in ROLES {
        let Some((_, items)) = blocks.iter().find(|(r, _)| r == role) else { continue };
        let rel = rel_path(role);
        let Ok(old) = safe_fs::read_regular(root, &rel, ((MAX_PROFILE_BYTES + MAX_LESSONS_SECTION_BYTES) * 2) as u64) else { continue };
        let lessons = clean_lessons(&items.iter().map(|l| scrub_text(secrets, l)).collect::<Vec<_>>());
        let Some(new) = with_lessons(&old, &lessons, spec_id, &today) else { continue };
        if new == old {
            continue;
        }
        if let Err(why) = validate_with(role, &new, Some(&old), true) {
            eprintln!("aviso: lições do perfil {role} descartadas ({spec_id}): {why}");
            continue;
        }
        changed.push((rel, old, new));
    }
    if changed.is_empty() {
        return Ok(0);
    }
    let mut written = 0;
    for (rel, _, new) in &changed {
        if super::approvals::replace_atomic(root, rel, new.as_bytes()).is_err() {
            restore(root, &changed[..written]);
            return Err(Skip("falha_ao_gravar"));
        }
        written += 1;
    }
    let paths: Vec<String> = changed.iter().map(|(rel, _, _)| rel.clone()).collect();
    let msg = crate::i18n::commit_subject(lang, &format!("docs({spec_id}): "), "commit.lessons");
    if Git::new(root).commit_paths(&paths, &msg).is_err() {
        restore(root, &changed);
        return Err(Skip("falha_no_commit"));
    }
    Ok(changed.len())
}

fn restore(root: &Path, files: &[(String, String, String)]) {
    for (rel, old, _) in files {
        let _ = super::approvals::replace_atomic(root, rel, old.as_bytes());
    }
}

/// Valida `answer`, grava atomicamente e commita só o `ARCHITECTURE.md`. Síncrono (testável).
/// O assunto do commit mantém o prefixo `docs(<spec>): ` e leva o texto no idioma do projeto.
pub fn apply(lang: crate::i18n::Lang, root: &Path, spec_id: &str, answer: &str, secrets: &SecretSet) -> Result<Outcome, Skip> {
    let msg = crate::i18n::commit_subject(lang, &format!("docs({spec_id}): "), "commit.architecture_file");
    apply_with_message(lang, root, &msg, answer, secrets)
}

/// Como [`apply`], com a mensagem de commit explícita. Os títulos das seções são gravados no
/// idioma do projeto (spec 125), mesmo que a resposta os traga no outro.
pub(super) fn apply_with_message(
    lang: crate::i18n::Lang,
    root: &Path,
    msg: &str,
    answer: &str,
    secrets: &SecretSet,
) -> Result<Outcome, Skip> {
    let content = validate(&scrub_text(secrets, answer), Some(lang))?;
    let git = Git::new(root);
    if !git.is_clean_ignoring_sdd().unwrap_or(false) {
        return Err(Skip("base_suja"));
    }
    let previous = match fs::symlink_metadata(root.join(ARCH_FILE)) {
        Err(_) => None,
        Ok(_) => Some(crate::agents::arch_memory::read_raw(root).ok_or(Skip("arquivo_inseguro"))?),
    };
    if previous.as_deref() == Some(content.as_str()) {
        return Ok(Outcome::Unchanged);
    }
    write_atomic(root, &content).ok_or(Skip("falha_ao_gravar"))?;
    if git.commit_path(ARCH_FILE, msg).is_err() {
        // Desfaz a gravação: a base volta ao estado anterior.
        match previous {
            Some(old) => {
                let _ = write_atomic(root, &old);
            }
            None => {
                let _ = fs::remove_file(root.join(ARCH_FILE));
            }
        }
        return Err(Skip("falha_no_commit"));
    }
    Ok(Outcome::Committed)
}

/// Valida o conteúdo de um `ARCHITECTURE.md` (mesmas regras da atualização pós-spec) e devolve
/// o texto normalizado. Usado pela escrita em rascunho e pela aprovação do plano.
/// Aceita os títulos em pt ou en (spec 125).
pub fn validate_content(answer: &str) -> Result<String, &'static str> {
    validate(answer, None).map_err(|Skip(code)| code)
}

/// Como [`validate_content`], com os títulos das seções reescritos no idioma `lang` (spec 125).
pub fn validate_content_for(answer: &str, lang: crate::i18n::Lang) -> Result<String, &'static str> {
    validate(answer, Some(lang)).map_err(|Skip(code)| code)
}

/// Normaliza e valida a resposta: remove cerca de código ao redor, exige título `# `, as seções
/// fixas na ordem (títulos em pt ou en) e até 64 KiB. Devolve o conteúdo com uma quebra de linha
/// final e, com `lang`, os títulos das seções reescritos nesse idioma.
fn validate(answer: &str, lang: Option<crate::i18n::Lang>) -> Result<String, Skip> {
    let mut text = answer.trim();
    if let Some(rest) = text.strip_prefix("```") {
        let body = rest.split_once('\n').map_or("", |(_, b)| b);
        text = body.trim_end().strip_suffix("```").unwrap_or(body).trim();
    }
    if !text.starts_with("# ") {
        return Err(Skip("sem_titulo"));
    }
    let mut next = 0;
    for index in sections_in_order(text) {
        if index == next {
            next += 1;
        }
    }
    if next < ARCH_SECTION_KEYS.len() {
        return Err(Skip("secao_ausente"));
    }
    let text = match lang {
        Some(lang) => localize_headings(text, lang),
        None => text.to_string(),
    };
    let out = format!("{}\n", text.trim_end());
    if out.len() as u64 > ARCH_MAX_FILE_BYTES {
        return Err(Skip("grande_demais"));
    }
    Ok(out)
}

/// Escrita atômica: temporário exclusivo na raiz, `fsync` e `rename`.
pub(super) fn write_atomic(root: &Path, content: &str) -> Option<()> {
    let target = safe_fs::check_chain(root, ARCH_FILE).ok()?;
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = root.join(format!(".{ARCH_FILE}.{}.{seq}.tmp", process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(content.as_bytes())?;
            file.sync_all()
        });
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    if safe_fs::check_chain(root, ARCH_FILE).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    fs::rename(&tmp, &target)
        .map_err(|_| {
            let _ = fs::remove_file(&tmp);
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn repo(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("nants-archup-{tag}-{}", process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("mkdir");
        git(&p, &["init", "-q", "-b", "main"]);
        git(&p, &["config", "user.email", "t@t"]);
        git(&p, &["config", "user.name", "t"]);
        fs::write(p.join("a.txt"), "a\n").expect("w");
        git(&p, &["add", "."]);
        git(&p, &["commit", "-q", "-m", "init"]);
        p
    }

    fn valid() -> String {
        let mut s = String::from("# Proj\n");
        for sec in crate::agents::arch_memory::section_headings(Lang::PtBr) {
            s.push_str(&format!("\n{sec}\ntexto\n"));
        }
        s
    }

    fn none() -> SecretSet {
        SecretSet::new(Vec::new())
    }

    #[test]
    fn writes_and_commits_only_the_architecture_file() {
        let r = repo("ok");
        let out = apply(Lang::PtBr, &r, "028-x", &valid(), &none()).expect("apply");
        assert_eq!(out, Outcome::Committed);
        assert!(fs::read_to_string(r.join(ARCH_FILE)).expect("r").contains("## Stack"));
        assert!(git(&r, &["log", "-1", "--format=%s"]).contains("docs(028-x): atualiza ARCHITECTURE.md"));
        assert!(git(&r, &["status", "--porcelain"]).is_empty());
        // Segunda chamada com o mesmo conteúdo: sem alteração.
        assert_eq!(apply(Lang::PtBr, &r, "028-x", &valid(), &none()), Ok(Outcome::Unchanged));
    }

    /// Spec 115 T04 (C-09): em projeto en, o mesmo prefixo `docs(<spec>): ` e o texto em inglês.
    #[test]
    fn architecture_commit_keeps_the_prefix_in_english() {
        let r = repo("ok-en");
        assert_eq!(apply(Lang::En, &r, "028-x", &valid(), &none()), Ok(Outcome::Committed));
        assert_eq!(git(&r, &["log", "-1", "--format=%s"]).trim(), "docs(028-x): update ARCHITECTURE.md");
    }

    fn doc_in(lang: Lang) -> String {
        let mut s = String::from("# Proj\n");
        for (i, sec) in crate::agents::arch_memory::template_headings(lang).iter().enumerate() {
            s.push_str(&format!("\n{sec}\nconteudo-{i}\n"));
        }
        s
    }

    /// Spec 125: a validação aceita os títulos em pt ou en, e um documento antigo em pt continua
    /// válido; a ordem e a presença das sete seções seguem exigidas nos dois idiomas.
    #[test]
    fn validation_accepts_headings_in_both_languages() {
        for lang in [Lang::PtBr, Lang::En] {
            let doc = doc_in(lang);
            assert_eq!(validate_content(&doc), Ok(doc.clone()), "{lang:?}");
            assert_eq!(validate_content_for(&doc, lang), Ok(doc.clone()), "{lang:?}");
            let missing = doc.replace("\n## Stack\n", "\n## Outra\n");
            assert_eq!(validate_content(&missing), Err("secao_ausente"), "{lang:?}");
        }
        // Mistura de idiomas na ordem certa também vale (documento editado à mão).
        let mixed = doc_in(Lang::PtBr).replace("## Estrutura", "## Structure").replace("## Restrições", "## Constraints");
        assert!(validate_content(&mixed).is_ok());
        // Fora de ordem: recusa.
        let swapped = doc_in(Lang::En).replace("## Overview", "## TMP").replace("## Stack", "## Overview").replace("## TMP", "## Stack");
        assert_eq!(validate_content(&swapped), Err("secao_ausente"));
        // Título só dentro de bloco de código não conta.
        let fenced = doc_in(Lang::En).replace("\n## Constraints\n", "\n```\n## Constraints\n```\n");
        assert_eq!(validate_content(&fenced), Err("secao_ausente"));
        // Revisão (F-02): código na linha no meio de uma seção não abre bloco.
        let inline = doc_in(Lang::PtBr).replace("conteudo-1\n", "```cargo test``` roda os testes\n");
        assert_eq!(validate_content(&inline), Ok(inline.clone()));
        // Revisão (F-01): título mais longo não substitui a seção.
        let longer = doc_in(Lang::En).replace("## Decisions\n", "## Decisions made\n");
        assert_eq!(validate_content(&longer), Err("secao_ausente"));
    }

    /// Spec 125: num projeto en com o documento antigo em pt, a atualização grava os títulos em en
    /// e mantém o conteúdo; no projeto pt, o documento em pt fica igual.
    #[test]
    fn update_in_english_project_rewrites_portuguese_headings() {
        let r = repo("relang");
        assert_eq!(apply(Lang::En, &r, "125-x", &doc_in(Lang::PtBr), &none()), Ok(Outcome::Committed));
        let text = fs::read_to_string(r.join(ARCH_FILE)).expect("r");
        assert_eq!(text, doc_in(Lang::En));
        for i in 0..7 {
            assert!(text.contains(&format!("conteudo-{i}")), "{text}");
        }
        assert!(!text.contains("Visão geral") && !text.contains("Restrições"), "{text}");
        // Volta ao pt: títulos em pt de novo.
        assert_eq!(apply(Lang::PtBr, &r, "125-y", &text, &none()), Ok(Outcome::Committed));
        assert_eq!(fs::read_to_string(r.join(ARCH_FILE)).expect("r"), doc_in(Lang::PtBr));
        assert_eq!(apply(Lang::PtBr, &r, "125-z", &doc_in(Lang::PtBr), &none()), Ok(Outcome::Unchanged));
    }

    const PROFILE: &str = "---\nrole: developer\nstack: [clojure]\nupdated_at: 2026-10-16\nsource_spec: 063-x\n---\n## Papel\n- Clojure com deps.edn.\n## Notas do usuário\n- usar cljfmt\n";

    #[test]
    fn answer_is_split_into_architecture_and_lessons() {
        let ans = "# P\n## Stack\nx\n=== LICOES developer ===\n- a (T01/F-01)\n=== LICOES reviewer ===\n- b (T02)\n";
        let (arch, blocks) = split_lessons(ans);
        assert_eq!(arch, "# P\n## Stack\nx\n");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ("developer".to_string(), vec!["- a (T01/F-01)".to_string()]));
        let (all, none) = split_lessons("# P\nsó arquitetura\n");
        assert_eq!(all, "# P\nsó arquitetura\n");
        assert!(none.is_empty());
    }

    #[test]
    fn lessons_update_only_their_section_and_commit_only_profiles() {
        let r = repo("lessons");
        fs::create_dir_all(r.join(".sdd/skills")).expect("mkdir");
        fs::write(r.join(".sdd/skills/developer.md"), PROFILE).expect("w");
        git(&r, &["add", "-f", ".sdd/skills"]);
        git(&r, &["commit", "-q", "-m", "perfis"]);
        let blocks = vec![
            ("developer".to_string(), vec!["- usar `clojure -M:test` (T03/F-02)".to_string(), "- sem origem".to_string()]),
            ("reviewer".to_string(), vec!["- x (T01)".to_string()]), // sem arquivo: ignorado
        ];
        assert_eq!(apply_lessons(Lang::PtBr, &r, "064-y", &blocks, &none()).expect("ok"), 1);
        let text = fs::read_to_string(r.join(".sdd/skills/developer.md")).expect("r");
        assert!(text.contains("## Lições do projeto\n- usar `clojure -M:test` (T03/F-02)"), "{text}");
        assert!(!text.contains("sem origem") && text.contains("## Notas do usuário\n- usar cljfmt"), "{text}");
        assert!(git(&r, &["log", "-1", "--format=%s"]).contains("lições dos perfis de especialista"));
        assert!(git(&r, &["status", "--porcelain"]).trim().is_empty());
        assert!(!r.join(".sdd/skills/reviewer.md").exists());
        // 2ª rodada, spec diferente: lições novas geram novo commit (e preservam as notas).
        let round2 = vec![("developer".to_string(), vec!["- outra (T04/F-01)".to_string()])];
        assert_eq!(apply_lessons(Lang::PtBr, &r, "065-z", &round2, &none()).expect("ok"), 1);
        let t2 = fs::read_to_string(r.join(".sdd/skills/developer.md")).expect("r");
        assert!(t2.contains("- outra (T04/F-01)") && !t2.contains("T03/F-02") && t2.contains("- usar cljfmt"), "{t2}");
        // mesma spec de novo, lições diferentes: também aplica (reason repetido não bloqueia).
        let round3 = vec![("developer".to_string(), vec!["- terceira (T05)".to_string()])];
        assert_eq!(apply_lessons(Lang::PtBr, &r, "065-z", &round3, &none()).expect("ok"), 1);
        assert!(fs::read_to_string(r.join(".sdd/skills/developer.md")).expect("r").contains("- terceira (T05)"));
        assert_eq!(git(&r, &["log", "--format=%s"]).matches("lições dos perfis").count(), 3);
        // sem lições válidas: nada muda.
        let none_valid = vec![("developer".to_string(), vec!["- sem origem".to_string()])];
        assert_eq!(apply_lessons(Lang::PtBr, &r, "065-z", &none_valid, &none()).expect("ok"), 0);
    }

    #[test]
    fn dirty_base_does_not_commit() {
        let r = repo("dirty");
        fs::write(r.join("sujo.txt"), "x").expect("w");
        assert_eq!(apply(Lang::PtBr, &r, "s", &valid(), &none()), Err(Skip("base_suja")));
        assert!(!r.join(ARCH_FILE).exists());
        assert_eq!(git(&r, &["log", "--format=%s"]).trim(), "init");
    }

    #[test]
    fn untracked_sdd_dir_does_not_block_and_is_not_committed() {
        let r = repo("sdd");
        fs::create_dir_all(r.join(".sdd/specs")).expect("mkdir");
        fs::write(r.join(".sdd/specs/x.md"), "x").expect("w");
        assert_eq!(apply(Lang::PtBr, &r, "s", &valid(), &none()), Ok(Outcome::Committed));
        let files = git(&r, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.trim(), ARCH_FILE);
        assert!(git(&r, &["status", "--porcelain"]).contains(".sdd/"));
    }

    #[test]
    fn dirty_file_outside_sdd_still_blocks_with_sdd_present() {
        let r = repo("sdd2");
        fs::create_dir_all(r.join(".sdd")).expect("mkdir");
        fs::write(r.join(".sdd/s.yaml"), "x").expect("w");
        fs::write(r.join("a.txt"), "mudou\n").expect("w");
        assert_eq!(apply(Lang::PtBr, &r, "s", &valid(), &none()), Err(Skip("base_suja")));
    }

    #[test]
    fn invalid_content_writes_nothing() {
        let r = repo("invalid");
        assert!(apply(Lang::PtBr, &r, "s", "sem título\n## Stack\n", &none()).is_err());
        assert!(apply(Lang::PtBr, &r, "s", "# T\n## Visão geral\nso isso\n", &none()).is_err());
        let huge = format!("{}\n{}", valid(), "x".repeat(70 * 1024));
        assert_eq!(apply(Lang::PtBr, &r, "s", &huge, &none()), Err(Skip("grande_demais")));
        assert!(!r.join(ARCH_FILE).exists());
        assert!(git(&r, &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn fenced_answer_is_unwrapped_and_secrets_redacted() {
        let r = repo("fence");
        let body = valid().replace("texto", "chave sk-supersecret-123456");
        let secrets = SecretSet::new(vec!["sk-supersecret-123456".to_string()]);
        apply(Lang::PtBr, &r, "s", &format!("```markdown\n{body}```"), &secrets).expect("apply");
        let saved = fs::read_to_string(r.join(ARCH_FILE)).expect("r");
        assert!(saved.starts_with("# Proj"));
        assert!(!saved.contains("sk-supersecret-123456"));
    }

    #[test]
    fn commit_failure_restores_previous_state() {
        let r = repo("commitfail");
        // Hook que recusa o commit.
        let hook = r.join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("hook");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert_eq!(apply(Lang::PtBr, &r, "s", &valid(), &none()), Err(Skip("falha_no_commit")));
        assert!(!r.join(ARCH_FILE).exists());
        assert!(git(&r, &["status", "--porcelain"]).is_empty());
    }
}
