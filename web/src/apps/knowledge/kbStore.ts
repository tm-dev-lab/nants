/*
 * Lista da base de conhecimento por projeto (spec 134 T06), compartilhada pela janela
 * Conhecimento e pelo subtítulo vivo do shell. Carrega sob demanda (`ensureKb`) e se atualiza
 * pelos eventos `kb_updated` (`applyKbEvent`), sem polling.
 */
import { useEffect, useSyncExternalStore } from "react";
import type { ApiFailure } from "../../api/client";
import { getKb, type KbDoc, type KbList } from "../../api/kb";

export type KbState =
  | { status: "loading" }
  | { status: "error"; failure: ApiFailure }
  | { status: "ok"; list: KbList };

const LOADING: KbState = { status: "loading" };
const states = new Map<string, KbState>();
const inflight = new Set<string>();
/** Pedido de releitura que chegou com um GET em voo: ao terminar, sai mais um GET. */
const pending = new Set<string>();
const listeners = new Set<() => void>();

function notify(): void {
  for (const listener of [...listeners]) listener();
}

function set(projectId: string, state: KbState): void {
  states.set(projectId, state);
  notify();
}

/**
 * Recarrega a lista do projeto, um GET por vez. Pedido com um GET em voo (um `kb_updated`, um
 * envio) liga a marca "pendente"; ao terminar, o GET se repete enquanto houver marca. Cada resposta
 * é gravada (a tela não para numa sequência longa de eventos) e a última vence.
 */
export async function reloadKb(projectId: string): Promise<void> {
  if (inflight.has(projectId)) {
    pending.add(projectId);
    return;
  }
  inflight.add(projectId);
  try {
    do {
      pending.delete(projectId);
      const result = await getKb(projectId);
      set(projectId, result.ok ? { status: "ok", list: result.data } : { status: "error", failure: result.error });
    } while (pending.has(projectId));
  } finally {
    inflight.delete(projectId);
  }
}

/** Carrega a lista se ainda não há nada para o projeto (ou se a última leitura falhou). */
function ensureKb(projectId: string): void {
  if (states.get(projectId)?.status !== "ok") void reloadKb(projectId);
}

/** Troca ou acrescenta um documento na lista já carregada. */
export function upsertKbDoc(projectId: string, doc: KbDoc): void {
  const state = states.get(projectId);
  if (state?.status !== "ok") return;
  const docs = state.list.docs.some((d) => d.id === doc.id)
    ? state.list.docs.map((d) => (d.id === doc.id ? doc : d))
    : [...state.list.docs, doc];
  set(projectId, { status: "ok", list: { ...state.list, docs } });
}

export function removeKbDoc(projectId: string, id: string): void {
  const state = states.get(projectId);
  if (state?.status !== "ok") return;
  set(projectId, { status: "ok", list: { ...state.list, docs: state.list.docs.filter((d) => d.id !== id) } });
}

export function setKbOverviewFlag(projectId: string, hasOverview: boolean): void {
  const state = states.get(projectId);
  if (state?.status !== "ok") return;
  set(projectId, { status: "ok", list: { ...state.list, has_overview: hasOverview } });
}

/**
 * Evento `kb_updated`: os indicadores dependem da base inteira (duplicata, link interno), então a
 * lista é relida; só os projetos já carregados.
 */
export function applyKbEvent(projectId: string): void {
  if (states.has(projectId)) void reloadKb(projectId);
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Estado da lista do projeto (carrega na primeira leitura). */
export function useKb(projectId: string | null): KbState | null {
  useEffect(() => {
    if (projectId !== null) ensureKb(projectId);
  }, [projectId]);
  return useSyncExternalStore(subscribe, () => (projectId === null ? null : (states.get(projectId) ?? LOADING)));
}

/** Estado já carregado, sem disparar a carga (o subtítulo vivo do shell). */
export function useKbSnapshot(projectId: string | null): KbState | null {
  return useSyncExternalStore(subscribe, () => (projectId === null ? null : (states.get(projectId) ?? null)));
}

/** Estado atual, sem assinar (testes). */
export function kbStateOf(projectId: string): KbState | null {
  return states.get(projectId) ?? null;
}

/** Testes: esquece tudo. */
export function resetKbStore(): void {
  states.clear();
  inflight.clear();
  pending.clear();
  notify();
}
