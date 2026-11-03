/*
 * O que a aba Memória mostra (spec 140 T08), sem React:
 * - os temas na ordem do servidor (a do índice), só os que têm nota ou um arquivo com problema;
 * - as apagadas à parte, a que sai primeiro antes;
 * - o runbook em partes: o texto em prosa e o comando exato em bloco de código (C-23).
 */
import { MEMORY_TOPICS, type MemoryEntry, type MemoryLimits, type MemoryTopic, type MemoryTopicView, type MemoryView } from "./nantyMemoryApi";

interface MemorySection {
  topic: MemoryTopic;
  status: MemoryTopicView["status"];
  entries: MemoryEntry[];
}

interface MemoryLayout {
  sections: MemorySection[];
  deleted: MemoryEntry[];
}

/**
 * As seções por tema (as em uso primeiro, as fora do índice no fim; em cada grupo, das mais antigas às
 * mais novas) e as apagadas que ainda voltam.
 */
export function memoryLayout(view: MemoryView): MemoryLayout {
  const sections: MemorySection[] = [];
  const deleted: MemoryEntry[] = [];
  for (const topic of MEMORY_TOPICS) {
    const found = view.topics.find((t) => t.topic === topic);
    if (found === undefined) continue;
    const entries = found.entries.filter((e) => e.state !== "deleted").sort((a, b) => Number(a.stale) - Number(b.stale) || a.created - b.created || a.id.localeCompare(b.id));
    deleted.push(...found.entries.filter((e) => e.state === "deleted"));
    const trouble = found.status === "newer" || found.status === "invalid";
    if (entries.length > 0 || trouble) sections.push({ topic, status: found.status, entries });
  }
  deleted.sort((a, b) => (a.restore_until_ms ?? 0) - (b.restore_until_ms ?? 0) || a.id.localeCompare(b.id));
  return { sections, deleted };
}

/** O tema aceita mudança: a instância é a dona e o arquivo é gravável. */
export function topicWritable(view: MemoryView, topic: MemoryTopic): boolean {
  const status = view.topics.find((t) => t.topic === topic)?.status ?? "missing";
  return view.writable && (status === "ok" || status === "missing");
}

/** O teto do texto no tema (o runbook tem o dele). */
export function textMax(topic: MemoryTopic, limits: MemoryLimits): number {
  return topic === "runbooks" ? limits.runbook_max_chars : limits.fact_max_chars;
}

type RunbookPart = { kind: "text" | "code"; text: string };

const FENCE = /```[^\n]*\n?([\s\S]*?)```/g;
const INLINE = /`([^`\n]+)`/g;

function split(source: string, pattern: RegExp): RunbookPart[] {
  const parts: RunbookPart[] = [];
  let last = 0;
  for (const match of source.matchAll(pattern)) {
    const at = match.index;
    const before = source.slice(last, at).trim();
    if (before.length > 0) parts.push({ kind: "text", text: before });
    const code = (match[1] ?? "").replace(/\n$/, "");
    if (code.trim().length > 0) parts.push({ kind: "code", text: code });
    last = at + match[0].length;
  }
  const rest = source.slice(last).trim();
  if (rest.length > 0) parts.push({ kind: "text", text: rest });
  return parts;
}

/**
 * O runbook em partes. Com blocos cercados por crases triplas, cada bloco vira código e o resto,
 * prosa; sem eles, cada trecho entre crases simples vira código. Sem nada marcado, o texto inteiro é
 * o comando: o runbook é o passo a passo exato, e nada se perde por adivinhar.
 */
export function runbookParts(source: string): RunbookPart[] {
  if (source.includes("```")) {
    const parts = split(source, FENCE);
    if (parts.some((p) => p.kind === "code")) return parts;
  }
  const inline = split(source, INLINE);
  if (inline.some((p) => p.kind === "code")) return inline;
  return [{ kind: "code", text: source }];
}
