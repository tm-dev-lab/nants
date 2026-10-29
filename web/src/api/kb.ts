/*
 * Cliente da base de conhecimento do projeto (spec 134; rotas em `src/server/kb.rs`):
 * listar, enviar arquivo (octet-stream), registrar link, ler a nota, editar a descrição, remover,
 * e ler e gravar a visão geral; desde a spec 136 T03, ler uma seção, buscar com os destaques e as
 * fontes da base citadas no plano. Sem classificação manual (spec 136 T02): o servidor não manda nem
 * recebe mais a marcação e o "Vale para". Todo erro do servidor tem
 * `code` (`serverError.kb_*`); o motivo da extração vem em `status_reason` (`kb.reason.*`).
 */
import { request, type ApiResult } from "./client";

type KbKind = "markdown" | "text" | "pdf" | "link";
export type KbStatus = "pending" | "extracting" | "ready" | "no_text" | "error";

/**
 * Indicador calculado pelo servidor, sem modelo (C-24), codificado: `duplicate` (`params.doc`: o
 * documento com o mesmo conteúdo), `no_headings` (só se cita o documento inteiro) e `broken_link`
 * (`params.target`: o arquivo que o link interno pede e a base não tem). O "sem texto" é o próprio
 * `status: no_text`. A interface traduz (`kb.indicator.<code>`).
 */
export interface KbIndicator {
  code: string;
  params?: Record<string, string | number>;
}

export interface KbDoc {
  id: string;
  title: string;
  kind: KbKind;
  source: "file" | "url" | "written";
  file_name: string | null;
  source_url: string | null;
  size: number;
  /** Impressão do texto atual: a versão (`base_sha256`) da edição (spec 136 T10). */
  sha256?: string;
  added_at: string;
  fetched_at: string | null;
  status: KbStatus;
  status_reason?: string | null;
  pages: number | null;
  description: string;
  links_out: string[];
  indicators?: KbIndicator[];
}

export interface KbList {
  docs: KbDoc[];
  has_overview: boolean;
  overview_invalid?: boolean;
}

/** Seção da nota (âncora da citação `kb:<id>#<âncora>`, título e nível). */
export interface KbSection {
  anchor: string;
  title: string;
  level: number;
  /** Linha do cabeçalho no `content` (a partir de 0). */
  line?: number;
}

export interface KbNote {
  doc: KbDoc;
  content: string;
  /** Seções da nota, para achar a seção citada (proposta da T06 para o `GET /kb/docs/:doc`). */
  sections?: KbSection[];
  /** "Links para" (spec 136 T03): os documentos da base e os alvos fora dela. */
  links_to?: KbLinksTo;
  /** "Citado por" (spec 136 T03), com a linha em volta do link, redigida e com teto. */
  cited_by?: KbCitedBy[];
  /** "Usado nos planos" (spec 136 T03): as specs que citam o documento. */
  used_in_plans?: KbPlanUsage[];
}

/** Documento da base numa lista de links. */
interface KbLinkedDoc {
  id: string;
  title: string;
  status: KbStatus;
}

interface KbLinksTo {
  docs: KbLinkedDoc[];
  /** Alvos que não levam a nenhum documento da base: um endereço ou um nome de arquivo. */
  outside: { kind: "url" | "file"; target: string }[];
}

interface KbCitedBy extends KbLinkedDoc {
  /** A linha do documento que cita, com o link; `null` quando o texto não a traz (página, PDF). */
  context: string | null;
}

/**
 * Um lugar em que o plano cita a base: a spec, uma tarefa (`task`) ou só a versão congelada na
 * aprovação (`frozen`, sem seção). `section` é o título da seção de topo (`null` antes do primeiro
 * título); `structural` diz se é um título estrutural (o mesmo nos dois idiomas, traduzido pela
 * interface); `binding` diz se a citação está numa seção de regra (critério, Premissas,
 * Orientações por papel). Fora disso, a citação é "descritiva".
 */
export interface KbCitationUse {
  in: "spec" | "task" | "frozen";
  task?: string;
  section: string | null;
  structural: boolean;
  binding: boolean;
}

/** Uso do documento num plano (`GET /kb/docs/:doc`, `used_in_plans`). */
interface KbPlanUsage {
  spec: string;
  title: string | null;
  /** A spec foi aprovada com este documento citado (`.sdd/kb/frozen`). */
  frozen: boolean;
  uses: (KbCitationUse & { anchor: string | null })[];
}

/**
 * Um resultado da busca (`GET /kb/search?q=&limit=`, spec 136 T03; formato em
 * `src/server/kb_read.rs`): a seção achada e o trecho já redigido, com os destaques em pares
 * `[início, fim)` de posições de caractere Unicode do `snippet` (use `Array.from(snippet)`),
 * calculados pelo servidor com a dobra de acentos do índice; a interface não reimplementa o
 * tokenizador e pinta os intervalos como texto (C-10).
 */
export interface KbSearchHit {
  doc: string;
  doc_title: string;
  /** Âncora da seção achada (a citação é `kb:<doc>#<anchor>`). */
  anchor: string;
  title: string;
  /** Caminho da seção, do documento até ela, unido por `" > "` (`kb::index::TRAIL_SEP`). */
  trail: string;
  page: number | null;
  snippet: string;
  highlights: [number, number][];
}

/** Seção de um documento (`GET /kb/docs/:doc?section=`), redigida e com teto. */
interface KbSectionText {
  anchor: string;
  title: string;
  /** Títulos acima da seção, do documento até ela. */
  trail: string[];
  /** Página do PDF em que a seção começa; `null` fora de PDF. */
  page: number | null;
  text: string;
  truncated: boolean;
  max_bytes: number;
}

/**
 * Uma fonte citada no plano (`GET /specs/:spec_id/kb-sources`): a citação, o documento, a seção
 * resolvida pela régua do portão e os lugares em que aparece. `binding` diz se algum uso está numa
 * seção de regra (C-04); `removed`, se o documento ou a seção saiu da base depois de citado.
 */
export interface KbPlanSource {
  citation: string;
  doc: string;
  doc_title: string | null;
  anchor: string | null;
  resolved_anchor: string | null;
  section_title: string | null;
  binding: boolean;
  removed: boolean;
  uses: KbCitationUse[];
}

interface KbOverview {
  text: string;
  max_bytes: number;
}

export interface KbPatch {
  description?: string;
}

const base = (projectId: string): string => `/api/projects/${encodeURIComponent(projectId)}/kb`;

/** O envio de arquivo pode levar mais que o prazo comum (até 32 MiB). */
const UPLOAD_TIMEOUT_MS = 120_000;

export function getKb(projectId: string): Promise<ApiResult<KbList>> {
  return request<KbList>({ method: "GET", path: base(projectId) });
}

export function postKbFile(projectId: string, file: Blob, name: string): Promise<ApiResult<{ doc: KbDoc }>> {
  return request<{ doc: KbDoc }>({
    method: "POST",
    path: `${base(projectId)}/files?name=${encodeURIComponent(name)}`,
    binary: { body: file, contentType: "application/octet-stream" },
    timeoutMs: UPLOAD_TIMEOUT_MS,
  });
}

export function postKbLink(projectId: string, url: string): Promise<ApiResult<{ doc: KbDoc }>> {
  return request<{ doc: KbDoc }>({ method: "POST", path: `${base(projectId)}/links`, body: JSON.stringify({ url }) });
}

export function getKbDoc(projectId: string, doc: string): Promise<ApiResult<KbNote>> {
  return request<KbNote>({ method: "GET", path: `${base(projectId)}/docs/${encodeURIComponent(doc)}` });
}

export function patchKbDoc(projectId: string, doc: string, patch: KbPatch): Promise<ApiResult<{ doc: KbDoc }>> {
  return request<{ doc: KbDoc }>({ method: "PATCH", path: `${base(projectId)}/docs/${encodeURIComponent(doc)}`, body: JSON.stringify(patch) });
}

export function deleteKbDoc(projectId: string, doc: string): Promise<ApiResult<{ removed: string }>> {
  return request<{ removed: string }>({ method: "DELETE", path: `${base(projectId)}/docs/${encodeURIComponent(doc)}` });
}

/** Cria um documento escrito no Nants (spec 136 T10): pronto e indexado na hora. */
export function postKbWritten(projectId: string, title: string, content: string): Promise<ApiResult<{ doc: KbDoc }>> {
  return request<{ doc: KbDoc }>({ method: "POST", path: `${base(projectId)}/written`, body: JSON.stringify({ title, content }) });
}

/**
 * Texto-fonte de um documento editável e a versão que o `PUT` espera (spec 136 T11). PDF e link →
 * 422 `kb_not_editable`; original que a redação mudaria → 422 `kb_source_redacted`.
 */
export function getKbContent(projectId: string, doc: string): Promise<ApiResult<{ content: string; sha256: string }>> {
  return request<{ content: string; sha256: string }>({ method: "GET", path: `${base(projectId)}/docs/${encodeURIComponent(doc)}/content` });
}

/**
 * Troca o texto de um documento editável (spec 136 T10). `baseSha256` é a versão aberta; outra
 * gravada antes dá 409 `kb_edit_conflict`.
 */
export function putKbContent(projectId: string, doc: string, content: string, baseSha256: string): Promise<ApiResult<{ doc: KbDoc }>> {
  return request<{ doc: KbDoc }>({
    method: "PUT",
    path: `${base(projectId)}/docs/${encodeURIComponent(doc)}/content`,
    body: JSON.stringify({ content, base_sha256: baseSha256 }),
  });
}

export function getKbOverview(projectId: string): Promise<ApiResult<KbOverview>> {
  return request<KbOverview>({ method: "GET", path: `${base(projectId)}/overview` });
}

export function putKbOverview(projectId: string, text: string): Promise<ApiResult<KbOverview>> {
  return request<KbOverview>({ method: "PUT", path: `${base(projectId)}/overview`, body: JSON.stringify({ text }) });
}

export function getKbSection(projectId: string, doc: string, anchor: string): Promise<ApiResult<{ doc: KbDoc; section: KbSectionText }>> {
  return request<{ doc: KbDoc; section: KbSectionText }>({
    method: "GET",
    path: `${base(projectId)}/docs/${encodeURIComponent(doc)}?section=${encodeURIComponent(anchor)}`,
  });
}

export function getKbSources(projectId: string, specId: string): Promise<ApiResult<{ sources: KbPlanSource[] }>> {
  return request<{ sources: KbPlanSource[] }>({
    method: "GET",
    path: `/api/projects/${encodeURIComponent(projectId)}/specs/${encodeURIComponent(specId)}/kb-sources`,
  });
}

export function searchKb(projectId: string, q: string, limit: number, signal?: AbortSignal): Promise<ApiResult<{ results: KbSearchHit[] }>> {
  return request<{ results: KbSearchHit[] }>({
    method: "GET",
    path: `${base(projectId)}/search?q=${encodeURIComponent(q)}&limit=${limit}`,
    signal,
  });
}
