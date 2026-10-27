/*
 * Pedido para abrir um arquivo nos Arquivos vindo de outra janela (spec 130: os arquivos que um
 * programa ou um comando do Console criou). O pedido fica guardado até a janela Arquivos o atender:
 * aberta, ela o lê na hora; fechada, ao montar.
 */
import { useSyncExternalStore } from "react";

interface OpenFileRequest {
  id: number;
  projectId: string;
  path: string;
}

let current: OpenFileRequest | null = null;
let seq = 0;
const listeners = new Set<() => void>();

function notify(): void {
  for (const listener of [...listeners]) listener();
}

/** Pede a abertura de `path` (relativo à raiz do projeto, na base) nos Arquivos. */
export function requestOpenFile(projectId: string, path: string): void {
  seq += 1;
  current = { id: seq, projectId, path };
  notify();
}

/** Marca o pedido como atendido. */
export function consumeOpenFile(id: number): void {
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

/** Pedido pendente para o projeto (ou `null`). */
export function useOpenFileRequest(projectId: string): OpenFileRequest | null {
  const request = useSyncExternalStore(subscribe, () => current);
  return request !== null && request.projectId === projectId ? request : null;
}
