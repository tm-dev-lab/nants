/*
 * Pedido para abrir um documento (e uma seção) no Conhecimento vindo de outra janela: a citação
 * `kb:` do Plano (spec 134 T06, critério 2). Fica guardado até a janela o atender: aberta, ela o
 * lê na hora; fechada, ao montar.
 */
import { useSyncExternalStore } from "react";

interface OpenDocRequest {
  id: number;
  projectId: string;
  doc: string;
  section: string | null;
}

let current: OpenDocRequest | null = null;
let seq = 0;
const listeners = new Set<() => void>();

function notify(): void {
  for (const listener of [...listeners]) listener();
}

export function requestOpenKbDoc(projectId: string, doc: string, section: string | null): void {
  seq += 1;
  current = { id: seq, projectId, doc, section };
  notify();
}

export function consumeOpenKbDoc(id: number): void {
  if (current?.id !== id) return;
  current = null;
  notify();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useOpenKbDocRequest(projectId: string): OpenDocRequest | null {
  const request = useSyncExternalStore(subscribe, () => current);
  return request !== null && request.projectId === projectId ? request : null;
}
