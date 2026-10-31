/*
 * Estado do Nanty compartilhado pela aba (spec 138 T04 e T10):
 * - o status (estado do avatar) e o feed (cartões, feito, previsto e o resumo de hoje), lidos na
 *   primeira assinatura de `useNanty`: o shell os lê para o selo e o subtítulo do menu do N;
 * - a conversa e o gasto (`useNantyConversation` e `useNantyUsage`), lidos só quando a janela do
 *   Nanty ou a janela Custos os pedem, num snapshot à parte (a conversa não re-renderiza o shell).
 *
 * Tudo se atualiza pelo evento `nanty_changed` do fluxo global (`applyNantyEvent`, com a `part` que
 * mudou) e nas ressincronizações (`resyncNanty`), sem polling. Um GET por parte de cada vez: pedido
 * com um GET em voo marca "pendente" e repete no fim.
 */
import { useEffect, useSyncExternalStore } from "react";
import {
  getNantyConversation,
  getNantyFeed,
  getNantyStatus,
  getNantyUsage,
  type NantyFeed,
  type NantyMessage,
  type NantyStatus,
  type NantyUsage,
} from "./nantyApi";

export interface NantySnapshot {
  status: NantyStatus | null;
  feed: NantyFeed | null;
}

interface ExtraSnapshot {
  conversation: NantyMessage[] | null;
  usage: NantyUsage | null;
}

type Part = "status" | "feed" | "conversation" | "usage";

const EMPTY: NantySnapshot = { status: null, feed: null };
const EMPTY_EXTRA: ExtraSnapshot = { conversation: null, usage: null };
let snapshot: NantySnapshot = EMPTY;
let extra: ExtraSnapshot = EMPTY_EXTRA;
let started = false;
let conversationStarted = false;
let usageStarted = false;
/** Geração: um `reset` descarta as respostas de GETs anteriores a ele. */
let generation = 0;
const inflight = new Set<Part>();
const pending = new Set<Part>();
const listeners = new Set<() => void>();
const extraListeners = new Set<() => void>();

function notify(set: Set<() => void>): void {
  for (const listener of [...set]) listener();
}

/** Lê uma parte e devolve se a resposta ainda vale (mesma geração). */
async function fetchPart(part: Part, gen: number): Promise<boolean> {
  // Leitura que falhou mantém o último estado conhecido.
  if (part === "status") {
    const result = await getNantyStatus();
    if (gen !== generation) return false;
    if (result.ok) snapshot = { ...snapshot, status: result.data };
    notify(listeners);
  } else if (part === "feed") {
    const result = await getNantyFeed();
    if (gen !== generation) return false;
    if (result.ok) snapshot = { ...snapshot, feed: result.data };
    notify(listeners);
  } else if (part === "conversation") {
    const result = await getNantyConversation();
    if (gen !== generation) return false;
    if (result.ok) extra = { ...extra, conversation: result.data };
    notify(extraListeners);
  } else {
    const result = await getNantyUsage();
    if (gen !== generation) return false;
    if (result.ok) extra = { ...extra, usage: result.data };
    notify(extraListeners);
  }
  return true;
}

async function load(part: Part): Promise<void> {
  if (inflight.has(part)) {
    pending.add(part);
    return;
  }
  inflight.add(part);
  const gen = generation;
  try {
    do {
      pending.delete(part);
      if (!(await fetchPart(part, gen))) return;
    } while (pending.has(part));
  } finally {
    if (gen === generation) inflight.delete(part);
  }
}

/** Relê o status e o feed. */
function reloadNanty(): void {
  started = true;
  void load("status");
  void load("feed");
}

/** Relê a conversa (depois de um envio, sem esperar o evento). */
export function reloadNantyConversation(): void {
  conversationStarted = true;
  void load("conversation");
}

/** Relê o feed e o status (depois de uma decisão, sem esperar o evento). */
export function reloadNantyFeed(): void {
  reloadNanty();
}

/** `lagged`, reconexão e retomada da aba: relê tudo o que a aba já leu. */
export function resyncNanty(): void {
  if (started) reloadNanty();
  if (conversationStarted) void load("conversation");
  if (usageStarted) void load("usage");
}

/**
 * `nanty_changed` com a `part`: o feed muda também o avatar (cartão aberto = esperando) e o gasto
 * (o turno e a voz gravam o gasto no estado); a conversa relê só a conversa.
 */
export function applyNantyEvent(part: string): void {
  if (part === "feed") {
    if (started) {
      void load("feed");
      void load("status");
    }
    if (usageStarted) void load("usage");
  } else if (part === "conversation") {
    if (conversationStarted) void load("conversation");
  } else if (part === "avatar" || part === "status") {
    if (started) void load("status");
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function subscribeExtra(listener: () => void): () => void {
  extraListeners.add(listener);
  return () => {
    extraListeners.delete(listener);
  };
}

/** O estado do Nanty; a primeira assinatura da aba dispara a leitura. */
export function useNanty(): NantySnapshot {
  useEffect(() => {
    if (!started) reloadNanty();
  }, []);
  return useSyncExternalStore(subscribe, () => snapshot, () => EMPTY);
}

/** A conversa (`null` antes da primeira leitura); a primeira assinatura a lê. */
export function useNantyConversation(): NantyMessage[] | null {
  useEffect(() => {
    if (!conversationStarted) reloadNantyConversation();
  }, []);
  return useSyncExternalStore(subscribeExtra, () => extra.conversation, () => null);
}

/** O gasto do Nanty (`null` antes da primeira leitura); a primeira assinatura o lê. */
export function useNantyUsage(): NantyUsage | null {
  useEffect(() => {
    if (!usageStarted) {
      usageStarted = true;
      void load("usage");
    }
  }, []);
  return useSyncExternalStore(subscribeExtra, () => extra.usage, () => null);
}

// --- Aba pedida de fora (T11: "Abrir o Nanty" em Configurações > Agentes abre os Ajustes) ---

type NantyTabRequest = { id: number; tab: "feed" | "rules" | "settings" };

let tabRequest: NantyTabRequest | null = null;
let tabRequestSeq = 0;
const tabListeners = new Set<() => void>();

/** Pede que a janela do Nanty mostre a aba (a janela aplica e consome o pedido). */
export function requestNantyTab(tab: NantyTabRequest["tab"]): void {
  tabRequestSeq += 1;
  tabRequest = { id: tabRequestSeq, tab };
  notify(tabListeners);
}

/** Consome o pedido aplicado (outro pedido no meio fica). */
export function consumeNantyTabRequest(id: number): void {
  if (tabRequest?.id !== id) return;
  tabRequest = null;
  notify(tabListeners);
}

function subscribeTab(listener: () => void): () => void {
  tabListeners.add(listener);
  return () => {
    tabListeners.delete(listener);
  };
}

/** O pedido de aba em aberto, se houver. */
export function useNantyTabRequest(): NantyTabRequest | null {
  return useSyncExternalStore(subscribeTab, () => tabRequest, () => null);
}

/** Testes: volta ao estado inicial (sem leitura feita). */
export function resetNantyStore(): void {
  tabRequest = null;
  generation += 1;
  snapshot = EMPTY;
  extra = EMPTY_EXTRA;
  started = false;
  conversationStarted = false;
  usageStarted = false;
  inflight.clear();
  pending.clear();
  notify(listeners);
  notify(extraListeners);
}
