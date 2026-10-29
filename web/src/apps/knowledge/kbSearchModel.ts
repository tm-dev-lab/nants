/*
 * Busca rápida da janela Conhecimento (spec 136 T07): o texto inteiro vai pelo servidor
 * (`GET /kb/search`), com espera curta, mínimo de letras, cancelamento do pedido anterior e
 * descarte das respostas fora de ordem. Os destaques chegam em posições de caractere Unicode do
 * trecho já redigido (C-10): aqui viram pedaços de texto, nunca HTML.
 */
import { useEffect, useState } from "react";
import { searchKb, type KbSearchHit } from "../../api/kb";
import { useKbGeneration } from "./kbStore";

/** Espera depois da última tecla antes de buscar no texto. */
export const KB_SEARCH_DEBOUNCE_MS = 200;
/** Letras mínimas para buscar no texto (o filtro de títulos roda desde a primeira). */
export const KB_SEARCH_MIN_CHARS = 2;
/** Resultados pedidos ao servidor. */
const KB_SEARCH_LIMIT = 8;

/** Um pedaço do trecho: texto comum ou um termo achado. */
interface SnippetPart {
  text: string;
  hit: boolean;
}

/**
 * Pedaços do trecho pelos destaques `[início, fim)` em posições de caractere Unicode
 * (`Array.from`): fora de ordem, sobrepostos ou fora do trecho, o intervalo é ajustado ou
 * descartado, e o texto sai inteiro, na ordem.
 */
export function snippetParts(snippet: string, highlights: readonly (readonly [number, number])[]): SnippetPart[] {
  const chars = Array.from(snippet);
  const ranges = highlights
    .map(([a, b]) => [Math.max(0, Math.trunc(a)), Math.min(chars.length, Math.trunc(b))] as const)
    .filter(([a, b]) => Number.isFinite(a) && Number.isFinite(b) && b > a)
    .sort((x, y) => x[0] - y[0]);
  const parts: SnippetPart[] = [];
  let at = 0;
  for (const [a, b] of ranges) {
    const start = Math.max(a, at);
    if (b <= start) continue;
    if (start > at) parts.push({ text: chars.slice(at, start).join(""), hit: false });
    parts.push({ text: chars.slice(start, b).join(""), hit: true });
    at = b;
  }
  if (at < chars.length) parts.push({ text: chars.slice(at).join(""), hit: false });
  return parts;
}

/** Separador do caminho da seção (`kb::index::TRAIL_SEP`). */
const TRAIL_SEP = " > ";

/** Títulos entre o documento e a seção achada (sem repetir um nem outro). */
export function hitTrail(hit: KbSearchHit): string[] {
  const parts = hit.trail.split(TRAIL_SEP).filter((p) => p.trim() !== "");
  if (parts[0] === hit.doc_title) parts.shift();
  if (parts[parts.length - 1] === hit.title) parts.pop();
  return parts;
}

type KbTextSearch =
  | { status: "idle" }
  | { status: "loading"; query: string; results: KbSearchHit[] }
  | { status: "ok"; query: string; results: KbSearchHit[] }
  | { status: "error"; query: string };

/**
 * Busca no texto da base: espera {@link KB_SEARCH_DEBOUNCE_MS} depois da última tecla, só com
 * {@link KB_SEARCH_MIN_CHARS} letras ou mais; cada pedido novo cancela o anterior
 * (`AbortController`) e uma resposta que chega depois de outra mais nova é descartada. Durante a
 * nova busca, os resultados anteriores ficam na tela (sem salto de layout).
 */
export function useKbTextSearch(projectId: string, query: string): KbTextSearch {
  const q = query.trim();
  const generation = useKbGeneration(projectId);
  const [state, setState] = useState<KbTextSearch>({ status: "idle" });
  useEffect(() => {
    if (Array.from(q).length < KB_SEARCH_MIN_CHARS) {
      setState({ status: "idle" });
      return undefined;
    }
    setState((prev) => ({ status: "loading", query: q, results: prev.status === "ok" || prev.status === "loading" ? prev.results : [] }));
    const controller = new AbortController();
    let live = true;
    const timer = setTimeout(() => {
      void searchKb(projectId, q, KB_SEARCH_LIMIT, controller.signal).then((result) => {
        if (!live) return;
        setState(result.ok ? { status: "ok", query: q, results: result.data.results } : { status: "error", query: q });
      });
    }, KB_SEARCH_DEBOUNCE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
      controller.abort();
    };
  }, [projectId, q, generation]);
  return state;
}
