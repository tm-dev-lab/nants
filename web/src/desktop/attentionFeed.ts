/*
 * Recarga de `/api/attention` (spec 093, decisão 7): pedida na abertura, no `lagged`, na
 * reconexão, na retomada da aba e por eventos relevantes, com no máximo 1 consulta a cada
 * 500 ms. Pedidos dentro da janela viram uma consulta só, no fim dela; um pedido durante a
 * consulta em voo agenda mais uma depois dela (o resultado nunca fica velho).
 */
import type { ApiEvent } from "../api/types";

const ATTENTION_MIN_INTERVAL_MS = 500;

/** Eventos que podem mudar o que espera o usuário. */
const RELEVANT: ReadonlySet<ApiEvent["event_type"]> = new Set<ApiEvent["event_type"]>([
  "permission_requested",
  "permission_resolved",
  "project_status",
  "task_updated",
  "sdd_draft_updated",
  "chat_turn_started",
  "chat_turn_ended",
  "recovery_started",
  "recovery_decided",
  "project_removed",
  "finalization_done",
]);

export function isAttentionEvent(event: ApiEvent): boolean {
  return RELEVANT.has(event.event_type);
}

interface AttentionFeedOptions<T> {
  load: () => Promise<T | null>;
  onData: (data: T) => void;
  minIntervalMs?: number;
  now?: () => number;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
}

export interface AttentionFeed {
  /** Pede uma consulta (com o limite de frequência). */
  request(): void;
  dispose(): void;
}

export function createAttentionFeed<T>(options: AttentionFeedOptions<T>): AttentionFeed {
  const min = options.minIntervalMs ?? ATTENTION_MIN_INTERVAL_MS;
  const now = options.now ?? ((): number => Date.now());
  const setTimer = options.setTimer ?? ((fn: () => void, ms: number): unknown => setTimeout(fn, ms));
  const clearTimer =
    options.clearTimer ?? ((handle: unknown): void => clearTimeout(handle as ReturnType<typeof setTimeout>));
  let lastStart: number | null = null;
  let timer: unknown = null;
  let inFlight = false;
  let dirty = false;
  let disposed = false;

  const run = (): void => {
    inFlight = true;
    lastStart = now();
    void options
      .load()
      .then((data) => {
        if (!disposed && data !== null) options.onData(data);
      })
      .catch(() => undefined)
      .finally(() => {
        inFlight = false;
        if (dirty && !disposed) {
          dirty = false;
          feed.request();
        }
      });
  };

  const feed: AttentionFeed = {
    request() {
      if (disposed) return;
      if (inFlight) {
        dirty = true;
        return;
      }
      if (timer !== null) return;
      // O relógio de parede pode voltar (ajuste do sistema; no WSL2 ele salta segundos para trás
      // sob carga): sem o teto, `lastStart` ficaria no "futuro" e a espera passaria do intervalo.
      // A espera fica sempre entre 0 e `min` (spec 107, T02).
      const wait = lastStart === null ? 0 : Math.min(min, Math.max(0, lastStart + min - now()));
      if (wait === 0) {
        run();
        return;
      }
      timer = setTimer(() => {
        timer = null;
        if (!disposed) run();
      }, wait);
    },
    dispose() {
      disposed = true;
      if (timer !== null) clearTimer(timer);
      timer = null;
    },
  };
  return feed;
}
