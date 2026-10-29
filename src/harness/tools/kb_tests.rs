//! Testes de `kb_search`/`kb_read` (spec 134 T04).

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::harness::allowlist::Policy;
use crate::harness::tools::{
    is_allowed, specs, specs_kb, ApprovalDecision, ApprovalRequest, Approver, Role as ToolRole, ToolHost, ToolName,
};
use crate::kb::{add_file, add_link, begin_extraction, extract, finish_extraction, update, Patch};

const NOW: &str = "2026-10-28T12:00:00Z";
// Montado em pedaços: a árvore pública recusa literal com cara de segredo.
const SECRET: &str = concat!("tok", "-kb-ferramenta-0123456789");

struct Deny;

impl Approver for Deny {
    fn approve<'a>(&'a self, _req: &'a ApprovalRequest) -> Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'a>> {
        Box::pin(async { ApprovalDecision::Deny })
    }
}

fn root(tag: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("nants-kbtool-{}-{n}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn none() -> SecretSet {
    SecretSet::new(Vec::<String>::new())
}

fn md(root: &Path, name: &str, text: &str) -> Entry {
    add_file(root, name, text.as_bytes(), &none(), NOW).expect("add")
}

fn access(root: &Path) -> KbAccess {
    KbAccess::open(root, None).expect("base pronta")
}

fn host(role: ToolRole, root: &Path) -> ToolHost {
    ToolHost::new(role, root.to_path_buf(), Vec::new(), Policy::new(Vec::new(), Vec::new()), Arc::new(Deny))
}

/// Link `ready` com a data de leitura: passa pela fila como a T02.
fn ready_link(root: &Path, url: &str, body: &str) -> Entry {
    let entry = add_link(root, url, &none(), NOW).expect("link");
    let begun = begin_extraction(root, &entry.id).expect("begin").expect("entrada");
    let ex = extract::text(body, true);
    let source = extract::LinkSource { ext: "html", bytes: b"<html></html>".to_vec() };
    finish_extraction(root, &entry.id, &begun.added_at, Ok((ex, Some(source))), &none(), "2026-10-27T09:30:00Z")
        .expect("finish")
        .expect("entrada")
}

#[test]
fn offer_needs_a_ready_document() {
    let root = root("offer");
    assert!(!has_ready(&root) && KbAccess::open(&root, None).is_none(), "sem base");
    assert!(KbAccess::open(Path::new(""), None).is_none(), "sem raiz");
    // Só pendente (PDF na fila): nada.
    add_file(&root, "manual.pdf", &crate::kb::test_pdf::pdf_with(&["Texto do manual."], true), &none(), NOW).expect("pdf");
    assert!(!has_ready(&root));
    md(&root, "regras.md", "# Regras\nUse filas.\n");
    assert!(has_ready(&root) && KbAccess::open(&root, None).is_some());
    // Manifesto ilegível: sem oferta.
    fs::write(root.join(crate::kb::MANIFEST_REL), "{").expect("corromper");
    assert!(!has_ready(&root));
}

#[test]
fn search_output_is_compact_and_marked() {
    let root = root("search");
    let doc = md(&root, "api.md", "# Padrões de API\n## Erros\nToda resposta de erro usa o envelope problem+json com o campo code.\n");
    let out = access(&root).search(&json!({"query": "envelope de erro"})).expect("busca");
    assert!(out.starts_with(KB_LABEL) && out.contains("DADO NÃO CONFIÁVEL"), "{out}");
    let line = out.lines().find(|l| l.starts_with("kb:")).expect("resultado");
    let fields: Vec<&str> = line.split(" | ").collect();
    assert_eq!(fields.len(), 5, "{line}");
    assert_eq!(fields[0], format!("kb:{}#erros", doc.id));
    assert_eq!(fields[1], "api", "o título do documento (sem frontmatter, o nome do arquivo)");
    assert_eq!(fields[2], "api > Padrões de API > Erros");
    assert_eq!(fields[3], "arquivo api.md");
    assert!(fields[4].contains("problem+json"), "{line}");
    // Sem resultado: uma linha sugerindo variar os termos, também marcada.
    let empty = access(&root).search(&json!({"query": "kubernetes"})).expect("busca");
    assert!(empty.starts_with(KB_LABEL) && empty.ends_with(NO_RESULTS), "{empty}");
    assert!(matches!(access(&root).search(&json!({"query": "  "})), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn search_limits_results_to_ten() {
    let root = root("limit");
    for i in 0..14 {
        md(&root, &format!("doc{i}.md"), &format!("# Doc {i}\nfilas de mensagens número {i}\n"));
    }
    let count = |args: serde_json::Value| access(&root).search(&args).expect("busca").lines().filter(|l| l.starts_with("kb:")).count();
    assert_eq!(count(json!({"query": "filas"})), DEFAULT_RESULTS);
    assert_eq!(count(json!({"query": "filas", "max_results": 3})), 3);
    assert_eq!(count(json!({"query": "filas", "max_results": 50})), MAX_RESULTS);
}

#[test]
fn only_standards_filters_by_role() {
    let root = root("standards");
    let reference = md(&root, "artigo.md", "# Artigo\nfilas com retry exponencial em todo serviço\n");
    let standard = md(&root, "padrao.md", "# Padrão\nfilas usam retry\n");
    let out = access(&root).search(&json!({"query": "filas retry", "only_standards": true})).expect("busca");
    assert!(out.ends_with(NO_STANDARD_RESULTS), "nenhum padrão ainda: {out}");
    update(&root, &standard.id, &Patch { role: Some(Role::Standard), ..Patch::default() }).expect("marcar");
    let all = access(&root).search(&json!({"query": "filas retry"})).expect("busca");
    assert!(all.contains(&reference.id) && all.contains(&standard.id), "{all}");
    let only = access(&root).search(&json!({"query": "filas retry", "only_standards": true})).expect("busca");
    assert!(only.contains(&standard.id) && !only.contains(&reference.id), "{only}");
    assert!(only.contains("| padrao [padrão] |"), "a marcação aparece no título: {only}");
    assert!(matches!(access(&root).search(&json!({"query": "filas", "only_standards": "sim"})), Err(ToolError::InvalidArgs(_))));
}

/// Revisão 1, F-02: o filtro de papel roda antes do teto do índice ([`index::MAX_RESULTS`]): um
/// padrão abaixo de mais de 50 seções de referência ainda aparece com `only_standards`.
#[test]
fn only_standards_finds_a_standard_below_many_references() {
    let root = root("standards-deep");
    for i in 0..(index::MAX_RESULTS + 6) {
        md(&root, &format!("ref{i}.md"), &format!("# Filas {i}\nfilas filas filas retry filas {i}\n"));
    }
    let filler = "texto longo sem relação com o assunto da busca ".repeat(40);
    let standard = md(&root, "padrao.md", &format!("# Norma\n{filler} filas\n"));
    update(&root, &standard.id, &Patch { role: Some(Role::Standard), ..Patch::default() }).expect("marcar");
    let all = index::search(&root, "filas", index::MAX_RESULTS, &none()).expect("busca");
    assert!(all.iter().all(|h| h.doc_id != standard.id), "o padrão fica abaixo do teto sem o filtro");
    let only = access(&root).search(&json!({"query": "filas", "only_standards": true})).expect("busca");
    assert!(only.contains(&format!("kb:{}#", standard.id)), "{only}");
    assert_eq!(only.lines().filter(|l| l.starts_with("kb:")).count(), 1, "{only}");
}

#[test]
fn read_returns_the_whole_section_and_the_index() {
    let root = root("read");
    let doc = md(&root, "guia.md", "# Guia\nIntro.\n## Deploy\nPasso um.\n### Rollback\nPasso dois.\n## Logs\nNível info.\n");
    let base = access(&root);
    let out = base.read(&json!({"id": doc.id, "section": "deploy"})).expect("seção");
    assert!(out.starts_with(KB_LABEL), "{out}");
    assert!(out.contains(&format!("kb:{}#deploy | guia | Guia > Deploy | arquivo guia.md", doc.id)), "{out}");
    assert!(out.contains("Passo um.") && out.contains("### Rollback") && out.contains("Passo dois."), "com as subseções: {out}");
    assert!(!out.contains("Nível info."), "{out}");
    // A citação inteira em `id` também vale.
    let cited = base.read(&json!({"id": format!("kb:{}#logs", doc.id)})).expect("citação");
    assert!(cited.contains("Nível info."), "{cited}");
    let list = base.read(&json!({"id": doc.id, "section": "list"})).expect("índice");
    assert!(list.starts_with(KB_LABEL) && list.contains("Índice (4 seções):"), "{list}");
    assert!(list.contains(&format!("- kb:{}#guia | Guia (", doc.id)), "{list}");
    assert!(list.contains(&format!("  - kb:{}#deploy | Deploy (", doc.id)), "{list}");
    assert!(list.contains(&format!("    - kb:{}#rollback | Rollback (", doc.id)), "recuo pelo nível: {list}");
    // Seção inexistente: a lista, marcada.
    let missing = base.read(&json!({"id": doc.id, "section": "nada"})).expect("lista");
    assert!(missing.starts_with(KB_LABEL) && missing.contains("Seção `nada` não encontrada") && missing.contains("#deploy"), "{missing}");
    // Documento fora da base, id inválido ou sem `section`: erro fixo.
    assert!(matches!(base.read(&json!({"id": "nao-existe-1", "section": "x"})), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(base.read(&json!({"id": "../x", "section": "x"})), Err(ToolError::InvalidArgs(_))));
    assert!(matches!(base.read(&json!({"id": doc.id})), Err(ToolError::InvalidArgs(_))));
}

#[test]
fn read_shows_links_inside_and_outside_the_base() {
    let root = root("links");
    let target = md(&root, "Regras API.md", "# Regras\nUse JSON.\n");
    let link = ready_link(&root, "https://exemplo.com/guia", "# Guia externo\nTexto.\n");
    let source = md(
        &root,
        "visao.md",
        "# Visão\n## Contratos\nVeja [as regras](./docs/Regras%20API.md#erros), [o guia](https://exemplo.com/guia) e [outro](https://fora.dev/x).\n## Outra\nSem links.\n",
    );
    let base = access(&root);
    let out = base.read(&json!({"id": source.id, "section": "contratos"})).expect("seção");
    assert!(out.contains(&format!("Links desta seção na base: kb:{}, kb:{}", target.id, link.id)), "{out}");
    assert!(out.contains("Links desta seção fora da base: https://fora.dev/x"), "{out}");
    // Seção sem links: sem as linhas.
    let plain = base.read(&json!({"id": source.id, "section": "outra"})).expect("seção");
    assert!(!plain.contains("Links desta seção"), "{plain}");
    // Quem cita o alvo.
    let cited = base.read(&json!({"id": target.id, "section": "regras"})).expect("seção");
    assert!(cited.contains(&format!("Citado por: kb:{}", source.id)), "{cited}");
    // O índice traz os links do documento.
    let list = base.read(&json!({"id": source.id, "section": "list"})).expect("índice");
    assert!(list.contains(&format!("Links deste documento na base: kb:{}, kb:{}", target.id, link.id)), "{list}");
    // Link: origem com a data de leitura.
    let page = base.read(&json!({"id": link.id, "section": "list"})).expect("índice");
    assert!(page.contains("https://exemplo.com/guia (lido em 2026-10-27)"), "{page}");
    let hit = base.search(&json!({"query": "guia externo"})).expect("busca");
    assert!(hit.contains("| https://exemplo.com/guia (lido em 2026-10-27) |"), "{hit}");
}

#[test]
fn a_big_section_is_cut_with_notice_and_keeps_the_footer() {
    let root = root("cut");
    let target = md(&root, "alvo.md", "# Alvo\nok\n");
    let para = "linha de texto do padrão que se repete para encher a seção inteira\n".repeat(600);
    let doc = md(&root, "grande.md", &format!("# Grande\nVeja [alvo](alvo.md).\n{para}"));
    let out = access(&root).read(&json!({"id": doc.id, "section": "grande"})).expect("seção");
    assert!(out.len() <= MAX_TOOL_RESULT_BYTES, "{}", out.len());
    assert!(out.starts_with(KB_LABEL) && out.contains("[seção cortada: "), "{}", &out[out.len() - 400..]);
    assert!(out.ends_with(&format!("Links desta seção na base: kb:{}", target.id)), "o rodapé chega: {}", &out[out.len() - 300..]);
    // Uma seção que cabe sai sem aviso.
    let small = access(&root).read(&json!({"id": target.id, "section": "alvo"})).expect("seção");
    assert!(!small.contains("cortada"), "{small}");
}

#[test]
fn neutral_anchor_never_collides_with_a_real_slug() {
    let root = root("anchor");
    let secrets = Arc::new(SecretSet::new(vec![SECRET.to_string()]));
    // O texto entra sem o segredo configurado (a base recusaria) e passa a ter um depois.
    let doc = md(&root, "chaves.md", &format!("## Chave {SECRET}\nconteúdo da chave\n## S 0\nconteúdo do S zero\n"));
    let base = KbAccess::open(&root, Some(Arc::clone(&secrets))).expect("base");
    let list = base.read(&json!({"id": doc.id, "section": "list"})).expect("índice");
    assert!(!list.contains(SECRET), "{list}");
    assert!(list.contains(&format!("kb:{}#s.0 |", doc.id)) && list.contains(&format!("kb:{}#s-0 |", doc.id)), "{list}");
    let neutral = base.read(&json!({"id": doc.id, "section": "s.0"})).expect("neutra");
    assert!(neutral.contains("conteúdo da chave") && !neutral.contains(SECRET), "{neutral}");
    let real = base.read(&json!({"id": doc.id, "section": "s-0"})).expect("slug");
    assert!(real.contains("conteúdo do S zero") && !real.contains("conteúdo da chave"), "{real}");
}

#[tokio::test]
async fn host_offers_and_runs_kb_only_with_a_ready_base() {
    let root = root("host");
    // As ferramentas nunca entram em `specs`; os três papéis podem usá-las.
    for role in [ToolRole::Architect, ToolRole::Developer, ToolRole::Reviewer] {
        assert!(specs(role).iter().all(|s| !s.name.starts_with("kb_")), "{role:?}");
        assert!(ToolName::KB.iter().all(|t| is_allowed(role, *t)), "{role:?}");
    }
    let names: Vec<String> = specs_kb().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["kb_search", "kb_read"]);
    // Base vazia: sem acesso; a chamada responde indisponível.
    let empty = host(ToolRole::Developer, &root).with_kb(&root, None);
    assert!(!empty.has_kb());
    let err = empty.call(ToolName::KbSearch, json!({"query": "x"})).await.expect_err("indisponível");
    assert!(err.to_string().contains("indisponível"), "{err}");
    md(&root, "a.md", "# A\nfilas\n");
    // A worktree do Developer não tem a base: a raiz que vale é a do projeto.
    let worktree = root.join("wt");
    fs::create_dir_all(&worktree).expect("wt");
    let dev = host(ToolRole::Developer, &worktree).with_kb(&root, None);
    assert!(dev.has_kb());
    let out = dev.call(ToolName::KbSearch, json!({"query": "filas"})).await.expect("busca");
    assert!(out.starts_with(KB_LABEL) && out.contains("kb:a-"), "{out}");
    assert!(!host(ToolRole::Developer, &worktree).with_kb(&worktree, None).has_kb(), "a worktree não tem base");
}
