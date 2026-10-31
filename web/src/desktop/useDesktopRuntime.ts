/*
 * Serviços do desktop montados UMA vez por aba (spec 093, decisão 7):
 * - uma única conexão SSE no fluxo global `/api/events` (filtrada por projeto no store), com a
 *   retomada da aba, o watchdog e o `lagged` do `liveStream`. O `project_removed` vai para a
 *   remoção do desktop (`apps/project/projectRemoval`), que avisa o shell;
 * - `pageVisibility` (pausa as animações com a aba oculta);
 * - `configLoader` (`config`/`configError` no store);
 * - `/api/attention`: na abertura, no `lagged`, na reconexão, na retomada e por eventos
 *   relevantes, com no máximo 1 consulta a cada 500 ms;
 * - `/api/readiness`: na abertura, na ressincronização e quando o shell pede;
 * - Nanty (spec 138 T04): o `nanty_changed` e as ressincronizações releem o status e o feed;
 * - Nanty (spec 138 T07): o aviso de aba visível (`nantyPresence`), que segura o Telegram.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { isConsoleEvent, publishConsoleEvent } from "../api/consoleEvents";
import { isDeviceEvent, publishDeviceEvent } from "../api/deviceEvents";
import { applyKbEvent } from "../apps/knowledge/kbStore";
import { applyNantyEvent, resyncNanty } from "../apps/nanty/nantyStore";
import { installNantyPresence } from "../apps/nanty/nantyPresence";
import type { ApiEvent } from "../api/types";
import { loadConfig } from "../configLoader";
import { handleLiveEvent, useLiveStream } from "../liveStream";
import { installPageVisibility } from "../pageVisibility";
import { handleSpaceRemovedEvent } from "../apps/project/projectRemoval";
import { getAttention, getReadiness, refreshReadiness, type Attention, type Readiness } from "./api";
import { sameJson } from "./uiState";
import { createAttentionFeed, isAttentionEvent, type AttentionFeed } from "./attentionFeed";

/** Resposta igual à anterior mantém a identidade (o shell não re-renderiza à toa). */
function keep<T>(prev: T | null, next: T): T {
  return prev !== null && sameJson(prev, next) ? prev : next;
}

export interface DesktopRuntime {
  attention: Attention | null;
  /** Pede uma nova consulta (com o limite de frequência). */
  refreshAttention: () => void;
  readiness: Readiness | null;
  /** Relê a prontidão (depois de salvar conexão ou agentes, por exemplo). */
  /** Relê a prontidão; devolve a resposta nova (ou `null` se a leitura falhou). */
  reloadReadiness: () => Promise<Readiness | null>;
  /** "Verificar de novo": confere a máquina no servidor e relê a prontidão. */
  recheckMachine: () => Promise<boolean>;
}

interface DesktopRuntimeOptions {
  /** Testes: intervalo mínimo entre consultas ao `/api/attention`. */
  attentionIntervalMs?: number;
}

export function useDesktopRuntime(options: DesktopRuntimeOptions = {}): DesktopRuntime {
  const [attention, setAttention] = useState<Attention | null>(null);
  const [readiness, setReadiness] = useState<Readiness | null>(null);
  const feedRef = useRef<AttentionFeed | null>(null);
  const intervalMs = options.attentionIntervalMs;

  const reloadReadiness = useCallback(async (): Promise<Readiness | null> => {
    const result = await getReadiness();
    if (!result.ok) return null;
    setReadiness((prev) => keep(prev, result.data));
    return result.data;
  }, []);
  const recheckMachine = useCallback(async (): Promise<boolean> => {
    const result = await refreshReadiness();
    if (result.ok) setReadiness((prev) => keep(prev, result.data));
    return result.ok;
  }, []);

  const refreshAttention = useCallback(() => feedRef.current?.request(), []);

  // Atenção: o alimentador (limite de frequência) vive enquanto o desktop existir.
  useEffect(() => {
    const feed = createAttentionFeed<Attention>({
      load: async () => {
        const result = await getAttention();
        return result.ok ? result.data : null;
      },
      onData: (next) => setAttention((prev) => keep(prev, next)),
      ...(intervalMs !== undefined ? { minIntervalMs: intervalMs } : {}),
    });
    feedRef.current = feed;
    feed.request();
    return () => {
      feed.dispose();
      if (feedRef.current === feed) feedRef.current = null;
    };
  }, [intervalMs]);

  // Config, prontidão, a classe da aba oculta e o aviso de aba visível do Nanty: uma vez.
  useEffect(() => {
    void loadConfig();
    void reloadReadiness();
    const offVisibility = installPageVisibility();
    const offPresence = installNantyPresence();
    return () => {
      offVisibility();
      offPresence();
    };
  }, [reloadReadiness]);

  // Fluxo global: uma conexão por aba.
  useLiveStream({
    onEvent: (event: ApiEvent) => {
      // Remoção: o shell fecha o espaço e avisa (nada de rota); o resto vai para o store.
      if (event.event_type === "project_removed") handleSpaceRemovedEvent(event.payload.project_id);
      else if (isDeviceEvent(event)) publishDeviceEvent(event);
      else if (isConsoleEvent(event)) publishConsoleEvent(event);
      // Base de conhecimento (spec 134 T06): a lista da janela Conhecimento se relê.
      else if (event.event_type === "kb_updated") {
        if (event.project_id !== null) applyKbEvent(event.project_id);
      }
      // Nanty (spec 138 T04): relê a parte que mudou (o avatar, o feed).
      else if (event.event_type === "nanty_changed") applyNantyEvent(event.payload.part)
      else handleLiveEvent(event);
      if (isAttentionEvent(event)) feedRef.current?.request();
    },
    onLagged: () => {
      feedRef.current?.request();
      resyncNanty();
    },
    onReconnected: () => {
      feedRef.current?.request();
      resyncNanty();
    },
    onResync: () => {
      feedRef.current?.request();
      resyncNanty();
      void reloadReadiness();
    },
  });

  return useMemo(
    () => ({ attention, refreshAttention, readiness, reloadReadiness, recheckMachine }),
    [attention, refreshAttention, readiness, reloadReadiness, recheckMachine],
  );
}
