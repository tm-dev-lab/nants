/*
 * Tratadores do fluxo GLOBAL de eventos (`/api/events`, sem project_id), que o desktop abre uma
 * única vez por aba (`useDesktopRuntime`). Alimentam o store para TODOS os projetos, então pedidos
 * de permissão e mensagens chegam mesmo com outro espaço aberto. Também ficam aqui a retomada da
 * aba e o agendador de ressincronização.
 */
import { useEffect, useRef } from "react";
import { useEventStream } from "./api/events";
import type { ApiEvent } from "./api/types";
import { appStore, bumpRevisions } from "./store";
import { refreshAllActivities } from "./activityRehydrate";
import { refreshDialogs } from "./dialogHistory";
import { loadChatHistory } from "./chatHistory";

/** Tempo mínimo oculta para que a volta à aba conte como retomada. */
const RESUME_MIN_HIDDEN_MS = 2_000;
/** Disparos de retomada dentro desta janela viram uma ressincronização só. */
export const RESUME_DEBOUNCE_MS = 300;

/**
 * Aplica um evento ao store. Eventos sem project_id são ignorados pelo redutor. Respostas do
 * Arquiteto chegam aqui com `turn_id` e são deduplicadas no store (não por agente).
 * O `project_removed` não passa por aqui: o desktop o trata (`useDesktopRuntime`).
 */
export function handleLiveEvent(event: ApiEvent): void {
  appStore.applyEvent(event);
}

/**
 * Evento `lagged` (assinante atrasou): não há como repor o que faltou no stream, então recarrega o
 * estado de todos os projetos e busca no diálogo persistido o que veio depois do último seq.
 */
function handleLagged(): void {
  bumpRevisions();
  void refreshDialogs();
}

/**
 * Stream reaberto depois de uma queda: pedidos enviados durante o intervalo não chegam por SSE,
 * então todos os projetos recarregam o estado (que traz `pending_permissions`).
 */
function handleReconnected(): void {
  bumpRevisions();
  void refreshAllActivities();
  void refreshDialogs();
}

/**
 * Ressincronização completa (retomada da aba ou watchdog): estado de todos os projetos, atividades,
 * diálogos e o histórico do chat do projeto aberto. O estado traz `architect_turn`: sem turno ativo,
 * o "pensando" some (ver `refreshProjectState`).
 */
function resyncAll(): void {
  handleReconnected();
  const active = appStore.getState().activeProjectId;
  if (active !== null && active !== undefined) void loadChatHistory(active);
}

interface ResumeOptions {
  /** Agendador compartilhado (debounce único entre retomada e watchdog). */
  scheduler: ResumeScheduler;
  doc?: Document;
  win?: Window;
  now?: () => number;
  minHiddenMs?: number;
}

/** Agendador com debounce: vários pedidos seguidos viram uma execução (reconecta se algum pediu). */
interface ResumeScheduler {
  schedule(reconnect: boolean): void;
  dispose(): void;
}

export function createResumeScheduler(
  reconnect: () => void,
  resync: () => void,
  debounceMs = RESUME_DEBOUNCE_MS,
): ResumeScheduler {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let wantReconnect = false;
  return {
    schedule(withReconnect) {
      wantReconnect = wantReconnect || withReconnect;
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        const doReconnect = wantReconnect;
        wantReconnect = false;
        if (doReconnect) reconnect();
        resync();
      }, debounceMs);
    },
    dispose() {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      wantReconnect = false;
    },
  };
}

/**
 * Retomada da aba (spec 080): `visibilitychange` para visível depois de ao menos `minHiddenMs`
 * oculta, `pageshow` vindo do cache (`persisted`) e `online` reabrem o stream e ressincronizam.
 * O navegador congela a aba oculta e o EventSource pode voltar zumbi ou sem ter disparado `onerror`.
 * Devolve o desinstalador (não descarta o agendador, que é do chamador).
 */
export function installResumeSync(options: ResumeOptions): () => void {
  const doc = options.doc ?? document;
  const win = options.win ?? window;
  // Mede só quanto tempo a aba ficou oculta: relógio monotônico, imune ao recuo do relógio de parede.
  const now = options.now ?? ((): number => performance.now());
  const minHidden = options.minHiddenMs ?? RESUME_MIN_HIDDEN_MS;
  const scheduler = options.scheduler;
  let hiddenAt: number | null = doc.visibilityState === "hidden" ? now() : null;

  const onVisibility = (): void => {
    if (doc.visibilityState === "hidden") {
      hiddenAt ??= now();
      return;
    }
    const since = hiddenAt;
    hiddenAt = null;
    if (since !== null && now() - since >= minHidden) scheduler.schedule(true);
  };
  const onPageShow = (event: PageTransitionEvent): void => {
    if (event.persisted) scheduler.schedule(true);
  };
  const onOnline = (): void => scheduler.schedule(true);

  doc.addEventListener("visibilitychange", onVisibility);
  win.addEventListener("pageshow", onPageShow);
  win.addEventListener("online", onOnline);
  return () => {
    doc.removeEventListener("visibilitychange", onVisibility);
    win.removeEventListener("pageshow", onPageShow);
    win.removeEventListener("online", onOnline);
  };
}

/** Ganchos extras do fluxo global (o desktop usa para a atenção, a prontidão e a remoção). */
export interface LiveStreamHooks {
  /** Substitui `handleLiveEvent` (quem passa decide o que vai para o store). */
  onEvent?: (event: ApiEvent) => void;
  /** Depois do tratamento padrão do `lagged`. */
  onLagged?: () => void;
  /** Depois do tratamento padrão da reconexão. */
  onReconnected?: () => void;
  /** Depois da ressincronização completa (retomada da aba ou watchdog). */
  onResync?: () => void;
}

/**
 * Mantém a assinatura global enquanto o chamador estiver montado: uma conexão, `lagged`,
 * reconexão, watchdog e retomada da aba (com debounce único).
 */
export function useLiveStream(hooks: LiveStreamHooks = {}): void {
  const hooksRef = useRef(hooks);
  hooksRef.current = hooks;
  const droppedRef = useRef(false);
  const schedulerRef = useRef<ResumeScheduler | null>(null);
  const stream = useEventStream(undefined, {
    onEvent: (event) => (hooksRef.current.onEvent ?? handleLiveEvent)(event),
    onLagged: () => {
      handleLagged();
      hooksRef.current.onLagged?.();
    },
    // O watchdog já reabriu a conexão: falta só ressincronizar (com o mesmo debounce da retomada).
    onWatchdog: () => schedulerRef.current?.schedule(false),
    onStatus: (status) => {
      if (status === "reconnecting") {
        droppedRef.current = true;
      } else if (status === "open" && droppedRef.current) {
        droppedRef.current = false;
        handleReconnected();
        hooksRef.current.onReconnected?.();
      }
    },
  });

  useEffect(() => {
    const scheduler = createResumeScheduler(
      () => stream.reconnectNow(),
      () => {
        resyncAll();
        hooksRef.current.onResync?.();
      },
    );
    schedulerRef.current = scheduler;
    const uninstall = installResumeSync({ scheduler });
    return () => {
      uninstall();
      scheduler.dispose();
      if (schedulerRef.current === scheduler) schedulerRef.current = null;
    };
  }, [stream]);
}
