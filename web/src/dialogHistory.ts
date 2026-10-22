/*
 * Diálogo dos agentes persistido (`GET /api/projects/:id/specs/:spec_id/dialog`) para o store.
 * - Carga completa (`loadDialog`): pagina em ordem crescente até a página vir menor que o limite.
 *   Guarda no máximo `DIALOG_MAX_MESSAGES` (as mais recentes) e substitui o diálogo daquela spec.
 * - Recuperação (`refreshDialogs`, após reconexão do SSE ou `lagged`): busca só o que vem depois do
 *   maior `dialogSeq` da spec e mescla por seq. Não refaz a carga inteira.
 * - Uma sequência anti-atraso por PROJETO: só a carga mais nova do projeto é aplicada. Além disso,
 *   a resposta só é aplicada se a spec ainda for a ativa do projeto (spec superada é descartada).
 * - Estado por (projeto, spec): `idle | loading | loaded | failed`. Reconexão durante `loading` marca
 *   uma recuperação para depois da carga; `failed` faz a próxima reconexão tentar a carga completa.
 *   Recuperação e carga da mesma spec nunca se sobrepõem (a recuperação roda depois da carga).
 * - Falha é devolvida ao chamador (a tela mostra a nota). Nunca apaga o que está na memória.
 */
import { getDialog } from "./api/client";
import type { DialogMessage } from "./api/types";
import { pickActiveSpec } from "./phase";
import { appStore, isProjectRemoved, maxDialogSeq } from "./store";

/** Linhas por página (o máximo que o servidor aceita). */
export const DIALOG_PAGE_LIMIT = 500;
/** Teto de mensagens lidas numa carga (as mais recentes). */
export const DIALOG_MAX_MESSAGES = 2000;
/** Páginas no máximo numa carga: proteção contra uma resposta que não avança. */
const MAX_PAGES = 200;

type DialogLoadOutcome = "applied" | "failed" | "stale";
type DialogStatus = "idle" | "loading" | "loaded" | "failed";

interface Slot {
  projectId: string;
  specId: string;
  status: DialogStatus;
  /** Reconexão/lagged chegou durante a carga: recupera depois de aplicá-la. */
  refreshPending: boolean;
  /** Uma recuperação está em andamento (novas pedidas viram `refreshAgain`). */
  refreshing: boolean;
  refreshAgain: boolean;
  /** Cargas completas deste slot ainda em voo (uma carga superada não apaga a recuperação de outra). */
  inflight: number;
}

/** Última carga emitida por projeto (sequência anti-atraso). */
const projectSeq = new Map<string, number>();
/** Estado por (projeto, spec). */
const slots = new Map<string, Slot>();

function keyOf(projectId: string, specId: string): string {
  return `${projectId}\u0000${specId}`;
}

function slotOf(projectId: string, specId: string): Slot {
  const key = keyOf(projectId, specId);
  let slot = slots.get(key);
  if (slot === undefined) {
    slot = {
      projectId,
      specId,
      status: "idle",
      refreshPending: false,
      refreshing: false,
      refreshAgain: false,
      inflight: 0,
    };
    slots.set(key, slot);
  }
  return slot;
}

/** Esquece as cargas (testes e troca de sessão). */
export function resetDialogHistory(): void {
  projectSeq.clear();
  slots.clear();
}

/** Spec ativa do projeto no store (`null` sem estado ou sem spec). */
export function activeSpecIdOf(projectId: string): string | null {
  const state = appStore.getState().projectStates[projectId];
  return pickActiveSpec(state?.specs ?? [])?.id ?? null;
}

/**
 * Lê as páginas a partir de `after`. Devolve `null` em qualquer falha. A janela devolvida tem no
 * máximo `DIALOG_MAX_MESSAGES` linhas, sempre as mais recentes.
 */
async function fetchFrom(projectId: string, specId: string, after: number): Promise<DialogMessage[] | null> {
  let cursor = after;
  let rows: DialogMessage[] = [];
  for (let page = 0; page < MAX_PAGES; page += 1) {
    // A primeira página não envia `after` (o servidor começa do início, como o padrão).
    const query = cursor === 0 ? { limit: DIALOG_PAGE_LIMIT } : { after: cursor, limit: DIALOG_PAGE_LIMIT };
    const result = await getDialog(projectId, specId, query);
    if (!result.ok) return null;
    const batch = result.data.messages;
    rows = rows.concat(batch);
    if (rows.length > DIALOG_MAX_MESSAGES) rows = rows.slice(rows.length - DIALOG_MAX_MESSAGES);
    const last = batch[batch.length - 1];
    if (batch.length < DIALOG_PAGE_LIMIT || last === undefined || last.seq <= cursor) break;
    cursor = last.seq;
  }
  return rows;
}

/** `true` quando a resposta de (projeto, spec) não deve mais ser aplicada. */
function isSuperseded(projectId: string, specId: string): boolean {
  return isProjectRemoved(appStore.getState(), projectId) || activeSpecIdOf(projectId) !== specId;
}

/**
 * Carrega o diálogo persistido da spec e o substitui no store. Devolve `"stale"` quando outra carga
 * do projeto foi iniciada, a spec deixou de ser a ativa ou o projeto foi removido durante a espera.
 */
export async function loadDialog(projectId: string, specId: string): Promise<DialogLoadOutcome> {
  const seq = (projectSeq.get(projectId) ?? 0) + 1;
  projectSeq.set(projectId, seq);
  const slot = slotOf(projectId, specId);
  slot.status = "loading";
  slot.inflight += 1;
  let rows: DialogMessage[] | null;
  try {
    rows = await fetchFrom(projectId, specId, 0);
  } finally {
    slot.inflight -= 1;
  }
  const sameLoad = projectSeq.get(projectId) === seq;
  if (!sameLoad || isSuperseded(projectId, specId)) {
    // Carga superada: só volta a `idle` quando nenhuma outra carga do projeto a substituiu.
    if (sameLoad && slot.status === "loading") slot.status = "idle";
    // Recuperação pendente só é descartada se não há outra carga do slot em voo (ou a spec saiu).
    if (slot.inflight === 0 || activeSpecIdOf(projectId) !== specId) slot.refreshPending = false;
    return "stale";
  }
  if (rows === null) {
    slot.status = "failed";
    slot.refreshPending = false;
    return "failed";
  }
  appStore.hydrateDialog(projectId, specId, rows);
  slot.status = "loaded";
  if (slot.refreshPending) {
    slot.refreshPending = false;
    await refreshOne(projectId, specId);
  }
  return "applied";
}

/**
 * Recupera o diálogo de cada spec conhecida do projeto. Chamado após reconexão do SSE e após
 * `lagged`: carga em voo marca recuperação para depois; falha refaz a carga completa; carregado
 * busca só o que veio depois do maior seq. Falha é silenciosa (o que já está na tela permanece).
 */
export async function refreshDialogs(): Promise<void> {
  const work: Promise<unknown>[] = [];
  for (const slot of [...slots.values()]) {
    if (isSuperseded(slot.projectId, slot.specId)) continue;
    if (slot.status === "loading") {
      slot.refreshPending = true;
    } else if (slot.status === "loaded") {
      work.push(refreshOne(slot.projectId, slot.specId));
    } else {
      work.push(loadDialog(slot.projectId, slot.specId));
    }
  }
  await Promise.all(work);
}

/** Recuperação serializada por (projeto, spec): pedidos durante uma recuperação viram uma repetição. */
async function refreshOne(projectId: string, specId: string): Promise<void> {
  const slot = slotOf(projectId, specId);
  if (slot.refreshing) {
    slot.refreshAgain = true;
    return;
  }
  slot.refreshing = true;
  try {
    do {
      slot.refreshAgain = false;
      await mergeSince(projectId, specId);
    } while (slot.refreshAgain);
  } finally {
    slot.refreshing = false;
  }
}

/** Uma busca após o maior seq da spec, mesclada se a spec ainda for a ativa. */
async function mergeSince(projectId: string, specId: string): Promise<void> {
  if (isSuperseded(projectId, specId)) return;
  const rows = await fetchFrom(projectId, specId, maxDialogSeq(appStore.getState(), projectId, specId));
  // Falha na recuperação é silenciosa por decisão: o diálogo já na tela permanece e a próxima
  // reconexão, `lagged` ou abertura da tela tenta de novo. A nota de falha é só da carga completa.
  if (rows === null) return;
  if (isSuperseded(projectId, specId)) return;
  appStore.mergeDialog(projectId, specId, rows);
}
