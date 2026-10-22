/*
 * Canal entre a Conversa e as outras janelas (spec 093, T06): o envio ao Arquiteto mora na
 * Conversa (mensagem otimista, `postChat`, dedupe por `turn_id`, acompanhamento do turno), e o
 * Plano pede por aqui o envio do "Pedir correção ao Arquiteto". Também expõe se há envio em curso
 * (o Plano desabilita o pedido enquanto isso).
 *
 * Pedidos ficam numa fila por projeto até a Conversa montada daquele projeto consumi-los.
 */
import { useSyncExternalStore } from "react";

interface ChatRequest {
  id: number;
  text: string;
}

const queues = new Map<string, ChatRequest[]>();
const sending = new Map<string, boolean>();
const listeners = new Set<() => void>();
let seq = 0;

function emit(): void {
  for (const listener of [...listeners]) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Pede o envio de `text` como mensagem do usuário na Conversa do projeto. */
export function requestChatSend(projectId: string, text: string): void {
  seq += 1;
  queues.set(projectId, [...(queues.get(projectId) ?? []), { id: seq, text }]);
  emit();
}

/** Tira e devolve o próximo pedido do projeto (a Conversa consome um por vez). */
export function takeChatRequest(projectId: string): ChatRequest | null {
  const queue = queues.get(projectId) ?? [];
  const [first, ...rest] = queue;
  if (first === undefined) return null;
  if (rest.length === 0) queues.delete(projectId);
  else queues.set(projectId, rest);
  emit();
  return first;
}

/** Quantos pedidos esperam a Conversa do projeto. */
function pendingChatRequests(projectId: string): number {
  return queues.get(projectId)?.length ?? 0;
}

/** A Conversa avisa quando um envio começa ou termina. */
export function setChatSending(projectId: string, value: boolean): void {
  if ((sending.get(projectId) ?? false) === value) return;
  if (value) sending.set(projectId, true);
  else sending.delete(projectId);
  emit();
}

function isChatSending(projectId: string): boolean {
  return sending.get(projectId) ?? false;
}

/** Envio em curso na Conversa do projeto. */
export function useChatSending(projectId: string): boolean {
  return useSyncExternalStore(subscribe, () => isChatSending(projectId), () => false);
}

/** Pedidos na fila do projeto (a Conversa reage a cada mudança). */
export function useChatRequestCount(projectId: string): number {
  return useSyncExternalStore(subscribe, () => pendingChatRequests(projectId), () => 0);
}
