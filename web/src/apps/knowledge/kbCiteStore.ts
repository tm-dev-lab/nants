/*
 * Leituras da citação (spec 136 T04): as fontes da base citadas no plano e o trecho de cada
 * citação para a prévia. O trecho fica guardado por sessão (critério 4): passar o mouse de novo na
 * mesma citação não chama o servidor. Um `kb_updated` (a geração da base, em `kbStore`) descarta os
 * trechos guardados do projeto e relê as fontes do plano.
 */
import { useEffect, useState } from "react";
import { getKbDoc, getKbSection, getKbSources, type KbPlanSource, type KbSection } from "../../api/kb";
import { kbGeneration, useKbGeneration } from "./kbStore";

/** Trecho mostrado na prévia: a seção citada, ou o começo do documento inteiro. */
interface KbExcerpt {
  title: string | null;
  trail: string[];
  page: number | null;
  text: string;
  truncated: boolean;
}

type ExcerptState = { status: "loading" } | { status: "ok"; excerpt: KbExcerpt } | { status: "error" };

/** Teto do começo do documento quando a citação não tem seção. */
const WHOLE_DOC_CHARS = 700;

const excerpts = new Map<string, Promise<ExcerptState>>();
/** Trechos já lidos, para a prévia abrir com o texto no primeiro quadro (sem piscar o carregamento). */
const ready = new Map<string, ExcerptState>();

function excerptKey(projectId: string, doc: string, anchor: string | null): string {
  return `${projectId}\n${kbGeneration(projectId)}\n${doc}\n${anchor ?? ""}`;
}

async function fetchExcerpt(projectId: string, doc: string, anchor: string | null): Promise<ExcerptState> {
  if (anchor !== null) {
    const result = await getKbSection(projectId, doc, anchor);
    if (!result.ok) return { status: "error" };
    const s = result.data.section;
    return { status: "ok", excerpt: { title: s.title, trail: s.trail, page: s.page, text: s.text, truncated: s.truncated } };
  }
  const result = await getKbDoc(projectId, doc);
  if (!result.ok) return { status: "error" };
  const content = result.data.content.replace(/^#[^\n]*\n+/, "");
  const cut = content.length > WHOLE_DOC_CHARS;
  let text = content;
  if (cut) {
    // Corta numa quebra de linha quando ela não fica cedo demais.
    const nl = content.lastIndexOf("\n", WHOLE_DOC_CHARS);
    text = content.slice(0, nl > WHOLE_DOC_CHARS / 2 ? nl : WHOLE_DOC_CHARS);
  }
  return { status: "ok", excerpt: { title: null, trail: [], page: null, text: text.trimEnd(), truncated: cut } };
}

function loadExcerpt(projectId: string, doc: string, anchor: string | null): Promise<ExcerptState> {
  const prefix = `${projectId}\n`;
  const current = `${prefix}${kbGeneration(projectId)}\n`;
  const key = excerptKey(projectId, doc, anchor);
  // Trechos de uma geração anterior da base saem do cache.
  for (const old of [...excerpts.keys()]) {
    if (old.startsWith(prefix) && !old.startsWith(current)) {
      excerpts.delete(old);
      ready.delete(old);
    }
  }
  const cached = excerpts.get(key);
  if (cached !== undefined) return cached;
  const pending = fetchExcerpt(projectId, doc, anchor).then((state) => {
    // Falha não fica guardada: a próxima prévia tenta de novo.
    if (state.status === "error") excerpts.delete(key);
    else if (excerpts.has(key)) ready.set(key, state);
    return state;
  });
  excerpts.set(key, pending);
  return pending;
}

/** Trecho da citação; só lê quando `enabled` (a prévia aberta). */
export function useKbExcerpt(projectId: string, doc: string, anchor: string | null, enabled: boolean): ExcerptState {
  const generation = useKbGeneration(projectId);
  const [state, setState] = useState<ExcerptState>(() => ready.get(excerptKey(projectId, doc, anchor)) ?? { status: "loading" });
  useEffect(() => {
    if (!enabled) return undefined;
    let alive = true;
    void loadExcerpt(projectId, doc, anchor).then((next) => {
      if (alive) setState(next);
    });
    return () => {
      alive = false;
    };
  }, [projectId, doc, anchor, enabled, generation]);
  return state;
}

/** Fontes do plano; `null` enquanto carrega ou se a leitura falhou (o quadro some). */
export function usePlanKbSources(projectId: string, specId: string, revision: string): KbPlanSource[] | null {
  const [sources, setSources] = useState<KbPlanSource[] | null>(null);
  const generation = useKbGeneration(projectId);
  useEffect(() => {
    let alive = true;
    void getKbSources(projectId, specId).then((result) => {
      if (alive) setSources(result.ok ? result.data.sources : null);
    });
    return () => {
      alive = false;
    };
  }, [projectId, specId, revision, generation]);
  return sources;
}

type SectionsState = { status: "loading" } | { status: "ok"; sections: KbSection[] } | { status: "error" };

/**
 * Sumários lidos (o segundo passo do seletor e o título da seção nas etiquetas, T06), por projeto,
 * geração e documento. Um pedido só por documento, mesmo com várias etiquetas montando juntas.
 */
const outlines = new Map<string, Promise<SectionsState>>();
const outlinesReady = new Map<string, SectionsState>();

function loadOutline(projectId: string, doc: string, key: string): Promise<SectionsState> {
  const cached = outlines.get(key);
  if (cached !== undefined) return cached;
  const pending = getKbDoc(projectId, doc).then((result): SectionsState => {
    if (!result.ok) {
      outlines.delete(key);
      return { status: "error" };
    }
    const state: SectionsState = { status: "ok", sections: result.data.sections ?? [] };
    outlinesReady.set(key, state);
    return state;
  });
  outlines.set(key, pending);
  return pending;
}

/** Seções do documento fora do React (o rascunho da Conversa refaz as etiquetas, T06). */
export function loadKbOutline(projectId: string, doc: string): Promise<SectionsState> {
  return loadOutline(projectId, doc, `${projectId}\n${kbGeneration(projectId)}\n${doc}`);
}

/** Seções do documento; lê uma vez por geração da base. `doc` nulo: nada. */
export function useKbSections(projectId: string, doc: string | null): SectionsState {
  const generation = useKbGeneration(projectId);
  const key = `${projectId}\n${generation}\n${doc ?? ""}`;
  const [state, setState] = useState<{ key: string; value: SectionsState } | null>(null);
  useEffect(() => {
    if (doc === null || outlinesReady.has(key)) return undefined;
    let alive = true;
    void loadOutline(projectId, doc, key).then((value) => {
      if (alive) setState({ key, value });
    });
    return () => {
      alive = false;
    };
  }, [projectId, doc, key]);
  return outlinesReady.get(key) ?? (state !== null && state.key === key ? state.value : { status: "loading" });
}

/** Testes: esquece os trechos guardados. */
export function resetKbExcerpts(): void {
  excerpts.clear();
  ready.clear();
  outlines.clear();
  outlinesReady.clear();
}
