/*
 * Cliente da base de conhecimento do projeto (spec 134; rotas em `src/server/kb.rs`):
 * listar, enviar arquivo (octet-stream), registrar link, ler a nota, editar a marcação, a
 * descrição e o `applies_to`, remover, e ler e gravar a visão geral. Todo erro do servidor tem
 * `code` (`serverError.kb_*`); o motivo da extração vem em `status_reason` (`kb.reason.*`).
 */
import { request, type ApiResult } from "./client";

type KbKind = "markdown" | "text" | "pdf" | "link";
export type KbStatus = "pending" | "extracting" | "ready" | "no_text" | "error";
export type KbRole = "standard" | "reference";

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
  source: "file" | "url";
  file_name: string | null;
  source_url: string | null;
  size: number;
  added_at: string;
  fetched_at: string | null;
  status: KbStatus;
  status_reason?: string | null;
  pages: number | null;
  role: KbRole;
  description: string;
  applies_to: string[];
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
}

interface KbOverview {
  text: string;
  max_bytes: number;
}

export interface KbPatch {
  role?: KbRole;
  description?: string;
  applies_to?: string[];
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

export function getKbOverview(projectId: string): Promise<ApiResult<KbOverview>> {
  return request<KbOverview>({ method: "GET", path: `${base(projectId)}/overview` });
}

export function putKbOverview(projectId: string, text: string): Promise<ApiResult<KbOverview>> {
  return request<KbOverview>({ method: "PUT", path: `${base(projectId)}/overview`, body: JSON.stringify({ text }) });
}
