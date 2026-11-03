/*
 * A memória do Nanty pela API (spec 140 T07 e T08), validada campo a campo:
 * - `GET /api/nanty/memory`: os temas (com a situação do arquivo), as entradas (inclusive as
 *   apagadas que ainda voltam, com `restore_until_ms`, e `stale` nas que saíram do índice) e os tetos;
 * - `PUT /api/nanty/memory/:id` com `{title, text, base_updated}`: a edição vale só sobre a versão
 *   lida; a outra vira 409 `nanty_memory_conflict`;
 * - `POST /api/nanty/memory/:id/delete` e `.../restore`;
 * - `POST /api/nanty/memory/export` (T08): a cópia para baixar.
 *
 * O texto das notas é sempre texto: a aba nunca o interpreta como HTML. O que não confere é
 * descartado.
 */
import { request, type ApiFailure, type ApiResult } from "../../api/client";
import { ptBR } from "../../i18n/ptBR";

export const MEMORY_TOPICS = ["identity", "working_together", "system", "runbooks", "decisions"] as const;
export type MemoryTopic = (typeof MEMORY_TOPICS)[number];

type MemorySourceKind = "person_said" | "model_clean" | "person_confirmed" | "imported" | "unknown";
type MemoryFileStatus = "ok" | "missing" | "newer" | "invalid";

export interface MemoryEntry {
  id: string;
  topic: MemoryTopic;
  title: string;
  text: string;
  source: { kind: MemorySourceKind; channel: "web" | "telegram" | null; project_id: string | null; at_ms: number };
  created: number;
  /** A versão da edição: a gravação só vale sobre ela. */
  updated: number;
  state: "active" | "deleted" | "unknown";
  /** Saiu do índice por falta de uso (continua aqui e no `recall`). */
  stale: boolean;
  /** Apagada: até quando volta. */
  restore_until_ms: number | null;
}

export interface MemoryTopicView {
  topic: MemoryTopic;
  status: MemoryFileStatus;
  entries: MemoryEntry[];
}

export interface MemoryLimits {
  title_max_chars: number;
  fact_max_chars: number;
  runbook_max_chars: number;
  deleted_keep_days: number;
}

export interface MemoryView {
  /** Esta instância é a dona: pode mudar. */
  writable: boolean;
  topics: MemoryTopicView[];
  limits: MemoryLimits;
}

const HEX_ID = /^[0-9a-f]{16}$/i;
const KINDS: readonly string[] = ["person_said", "model_clean", "person_confirmed", "imported"];
const STATUSES: readonly string[] = ["ok", "missing", "newer", "invalid"];
/** Teto de texto de uma nota lida (o runbook tem até 1.500; folga para tetos maiores). */
const MAX_TEXT = 16_384;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const num = (value: unknown): number | null => (typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null);
const text = (value: unknown, max: number): string | null => (typeof value === "string" && value.length > 0 && value.length <= max ? value : null);
const isTopic = (value: unknown): value is MemoryTopic => typeof value === "string" && (MEMORY_TOPICS as readonly string[]).includes(value);

function parseEntry(value: unknown): MemoryEntry | null {
  if (!isRecord(value) || !isRecord(value.source)) return null;
  const id = text(value.id, 16);
  const title = text(value.title, 1024);
  const body = text(value.text, MAX_TEXT);
  const updated = num(value.updated);
  const at = num(value.source.at_ms);
  if (id === null || !HEX_ID.test(id) || !isTopic(value.topic) || title === null || body === null || updated === null || at === null) return null;
  const source = value.source;
  const kind = typeof source.kind === "string" && KINDS.includes(source.kind) ? (source.kind as MemorySourceKind) : "unknown";
  const channel = source.channel === "web" || source.channel === "telegram" ? source.channel : null;
  const state = value.state === "active" || value.state === "deleted" ? value.state : "unknown";
  return {
    id,
    topic: value.topic,
    title,
    text: body,
    source: { kind, channel, project_id: text(source.project_id, 128), at_ms: at },
    created: num(value.created) ?? at,
    updated,
    state,
    stale: value.stale === true,
    restore_until_ms: state === "deleted" ? num(value.restore_until_ms) : null,
  };
}

export function parseMemoryView(value: unknown): MemoryView | null {
  if (!isRecord(value) || !Array.isArray(value.topics) || !isRecord(value.limits)) return null;
  const topics: MemoryTopicView[] = [];
  for (const raw of value.topics) {
    if (!isRecord(raw) || !isTopic(raw.topic)) continue;
    const status = typeof raw.status === "string" && STATUSES.includes(raw.status) ? (raw.status as MemoryFileStatus) : "invalid";
    const entries = Array.isArray(raw.entries) ? raw.entries.map(parseEntry).filter((e): e is MemoryEntry => e !== null && e.topic === raw.topic) : [];
    topics.push({ topic: raw.topic, status, entries });
  }
  const l = value.limits;
  const limits = {
    title_max_chars: num(l.title_max_chars) ?? 80,
    fact_max_chars: num(l.fact_max_chars) ?? 400,
    runbook_max_chars: num(l.runbook_max_chars) ?? 1500,
    deleted_keep_days: Math.max(1, num(l.deleted_keep_days) ?? 7),
  };
  return { writable: value.writable === true, topics, limits };
}

const INVALID: ApiFailure = { kind: "invalid_response", status: 200, message: ptBR["api.invalidResponse"], messageKey: "api.invalidResponse" };

function parsed<T>(parse: (value: unknown) => T | null) {
  return (r: ApiResult<unknown>): ApiResult<T> => {
    if (!r.ok) return r;
    const data = parse(r.data);
    return data === null ? { ok: false, error: { ...INVALID } } : { ok: true, data };
  };
}

const parseEntryReply = (value: unknown): MemoryEntry | null => (isRecord(value) ? parseEntry(value.entry) : null);

export function getNantyMemory(): Promise<ApiResult<MemoryView>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/memory" }).then(parsed(parseMemoryView));
}

export function putNantyMemory(id: string, title: string, body: string, baseUpdated: number): Promise<ApiResult<MemoryEntry>> {
  return request<unknown>({
    method: "PUT",
    path: `/api/nanty/memory/${encodeURIComponent(id)}`,
    body: JSON.stringify({ title, text: body, base_updated: baseUpdated }),
  }).then(parsed(parseEntryReply));
}

export function postNantyMemory(id: string, action: "delete" | "restore"): Promise<ApiResult<MemoryEntry>> {
  return request<unknown>({ method: "POST", path: `/api/nanty/memory/${encodeURIComponent(id)}/${action}` }).then(parsed(parseEntryReply));
}

/** A cópia da memória, como o servidor a monta (só as ativas, redigidas de novo). */
export function exportNantyMemory(): Promise<ApiResult<unknown>> {
  return request<unknown>({ method: "POST", path: "/api/nanty/memory/export" }).then(parsed((v) => (isRecord(v) && isRecord(v.topics) ? v : null)));
}
