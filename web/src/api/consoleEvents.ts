/*
 * Eventos do Console do projeto (spec 130): `app_exec_started`, `app_exec_output` e
 * `app_exec_finished` chegam pelo fluxo global (uma conexão por aba) e são repassados à sessão do
 * Console (`apps/app/consoleSession.ts`). A saída ao vivo não passa pelo store dos projetos (seria
 * um re-render de todas as janelas a cada linha).
 */
import type { ApiEvent } from "./types";

export type ConsoleEvent = Extract<ApiEvent, { event_type: "app_exec_started" | "app_exec_output" | "app_exec_finished" }>;

const listeners = new Set<(event: ConsoleEvent) => void>();

export function isConsoleEvent(event: ApiEvent): event is ConsoleEvent {
  return event.event_type === "app_exec_started" || event.event_type === "app_exec_output" || event.event_type === "app_exec_finished";
}

/** Assina os eventos do Console; devolve o cancelamento. */
export function subscribeConsoleEvents(listener: (event: ConsoleEvent) => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Entrega um evento aos assinantes. */
export function publishConsoleEvent(event: ConsoleEvent): void {
  for (const listener of [...listeners]) listener(event);
}
