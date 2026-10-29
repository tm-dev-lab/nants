/*
 * Filtro do seletor da base (spec 136 T06, C-11): o mesmo para o `@` da Conversa e para a busca
 * rápida da janela Conhecimento (T07). Roda no cliente, sobre a lista já carregada (`GET /kb`),
 * sem acento e sem diferença de caixa, e devolve os trechos casados do título para o destaque.
 */
import type { KbDoc, KbSection } from "../../api/kb";

/** Trecho casado `[início, fim)`, em posições do texto original. */
export type MatchRange = [number, number];

/** Texto dobrado (sem acento, minúsculo) e, para cada caractere dobrado, a posição no original. */
function foldWithMap(text: string): { folded: string; map: number[] } {
  let folded = "";
  const map: number[] = [];
  let at = 0;
  for (const ch of text) {
    const f = ch.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
    for (let i = 0; i < f.length; i += 1) {
      folded += f.charAt(i);
      map.push(at);
    }
    at += ch.length;
  }
  map.push(at);
  return { folded, map };
}

/** Sem acento e minúsculo (a comparação do filtro). */
export function foldText(text: string): string {
  return foldWithMap(text).folded;
}

function queryTokens(query: string): string[] {
  return foldText(query)
    .split(/\s+/)
    .filter((token) => token !== "");
}

/** Os trechos do título que casam com as palavras da consulta, ou `null` se alguma não casa. */
function matchTitle(title: string, tokens: readonly string[]): MatchRange[] | null {
  const { folded, map } = foldWithMap(title);
  const ranges: MatchRange[] = [];
  for (const token of tokens) {
    const at = folded.indexOf(token);
    if (at < 0) return null;
    ranges.push([map[at] ?? 0, map[at + token.length] ?? title.length]);
  }
  ranges.sort((a, b) => a[0] - b[0]);
  return ranges;
}

/** Origem do documento na lista: o nome do arquivo ou o domínio do link. */
export function docOrigin(doc: KbDoc): string {
  if (doc.source === "url" && doc.source_url !== null) {
    try {
      return new URL(doc.source_url).hostname.replace(/^www\./, "");
    } catch {
      return doc.source_url;
    }
  }
  return doc.file_name ?? "";
}

/** O documento pode ser mencionado: só o pronto (o servidor recusa o resto, T05). */
function mentionable(doc: KbDoc): boolean {
  return doc.status === "ready";
}

/** O documento tem seções para o segundo passo (sem o indicador `no_headings`). */
export function hasSections(doc: KbDoc): boolean {
  return !(doc.indicators ?? []).some((indicator) => indicator.code === "no_headings");
}

interface DocMatch {
  doc: KbDoc;
  /** Trechos casados do título (vazio quando casou só pela origem ou sem consulta). */
  ranges: MatchRange[];
}

/**
 * Documentos que casam com a consulta, os melhores primeiro: título que começa com a consulta,
 * palavra do título que começa com ela, título que a contém e, por fim, só a origem. No empate, a
 * ordem da lista da base. Sem consulta, todos, na ordem da lista.
 */
export function filterKbDocs(docs: readonly KbDoc[], query: string): DocMatch[] {
  const tokens = queryTokens(query);
  const pool = docs.filter(mentionable);
  if (tokens.length === 0) return pool.map((doc) => ({ doc, ranges: [] }));
  const whole = tokens.join(" ");
  const scored: { match: DocMatch; score: number; order: number }[] = [];
  pool.forEach((doc, order) => {
    const ranges = matchTitle(doc.title, tokens);
    const title = foldText(doc.title);
    if (ranges !== null) {
      const first = tokens[0] ?? "";
      const score = title.startsWith(whole) ? 0 : new RegExp(`(^|[^\\p{L}\\p{N}])${escapeRegExp(first)}`, "u").test(title) ? 1 : 2;
      scored.push({ match: { doc, ranges }, score, order });
      return;
    }
    const origin = foldText(docOrigin(doc));
    if (tokens.every((token) => origin.includes(token))) scored.push({ match: { doc, ranges: [] }, score: 3, order });
  });
  scored.sort((a, b) => a.score - b.score || a.order - b.order);
  return scored.map((entry) => entry.match);
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

interface SectionMatch {
  section: KbSection;
  ranges: MatchRange[];
}

/** Seções que casam com a consulta, na ordem do documento (o sumário mantém os níveis). */
export function filterKbSections(sections: readonly KbSection[], query: string): SectionMatch[] {
  const tokens = queryTokens(query);
  const out: SectionMatch[] = [];
  for (const section of sections) {
    if (tokens.length === 0) {
      out.push({ section, ranges: [] });
      continue;
    }
    const ranges = matchTitle(section.title, tokens);
    if (ranges !== null) out.push({ section, ranges });
  }
  return out;
}

/** O documento cujo título é exatamente o texto (sem acento nem caixa), para o passo da seção. */
export function docByTitle(docs: readonly KbDoc[], title: string): KbDoc | null {
  const wanted = foldText(title.trim());
  if (wanted === "") return null;
  return docs.find((doc) => mentionable(doc) && foldText(doc.title) === wanted) ?? null;
}
