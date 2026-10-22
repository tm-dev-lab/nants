/*
 * Gestos de mover (barra de título) e redimensionar (borda direita, inferior e canto).
 * Pointer events com `setPointerCapture`; durante o gesto, só estilo inline aplicado por rAF
 * (`transform` ao mover; largura/altura ao redimensionar) e nenhuma animação; o estado só muda
 * no `pointerup`, por `onCommit`. `pointercancel` desfaz a prévia sem gravar nada.
 */
import { useCallback, useEffect, useRef, useState, useSyncExternalStore, type PointerEvent as ReactPointerEvent, type RefObject } from "react";
import { gestureRect, gestureSignal, snapZoneAt, type GestureMode } from "./gestures";
import type { Rect, SnapTarget, Workspace } from "./windows";

export interface FrameScheduler {
  request(callback: () => void): number;
  cancel(id: number): void;
}

const defaultScheduler: FrameScheduler = {
  request: (callback) => requestAnimationFrame(() => callback()),
  cancel: (id) => cancelAnimationFrame(id),
};

export interface GestureCommit {
  rect: Rect;
  /** Encaixe escolhido ao soltar (só ao mover perto da borda). */
  snap: SnapTarget | null;
}

interface WindowGestureOptions {
  /** A janela: recebe o estilo inline durante o gesto. */
  windowRef: RefObject<HTMLElement | null>;
  /** Geometria atual (do estado). */
  rect: Rect;
  workspace: Workspace;
  /** Canto da área de trabalho no viewport, para a zona de encaixe. */
  workspaceOrigin?: () => { x: number; y: number };
  /** Desligado (maximizada, celular): nenhum gesto começa. */
  enabled?: boolean;
  onCommit: (commit: GestureCommit) => void;
  /** Primeiro movimento real do gesto (ex.: dica "Arraste até a borda para encaixar"). */
  onStart?: (mode: GestureMode) => void;
  scheduler?: FrameScheduler;
}

interface ActiveGesture {
  mode: GestureMode;
  pointerId: number;
  startX: number;
  startY: number;
  start: Rect;
  current: Rect;
  snap: SnapTarget | null;
  frame: number | null;
  target: HTMLElement;
  started: boolean;
  cleanup: () => void;
}

/** Controles interativos da barra de título e o slot de ferramentas (C-07) não iniciam arraste. */
const NO_DRAG_SELECTOR = "button, a, input, select, textarea, [data-no-drag], [data-window-tools]";

interface WindowGesture {
  /** `onPointerDown` da barra de título (mover). */
  onMovePointerDown: (event: ReactPointerEvent<HTMLElement>) => void;
  /** `onPointerDown` de uma alça de redimensionar. */
  onResizePointerDown: (mode: Exclude<GestureMode, "move">) => (event: ReactPointerEvent<HTMLElement>) => void;
  /** Encaixe sob o ponteiro (prévia translúcida); `null` fora das bordas. */
  snapPreview: SnapTarget | null;
  dragging: boolean;
}

function clearInline(element: HTMLElement | null): void {
  if (element === null) return;
  element.style.transform = "";
  element.style.width = "";
  element.style.height = "";
}

export function useWindowGesture(options: WindowGestureOptions): WindowGesture {
  const optionsRef = useRef(options);
  optionsRef.current = options;
  const activeRef = useRef<ActiveGesture | null>(null);
  const [snapPreview, setSnapPreview] = useState<SnapTarget | null>(null);
  const [dragging, setDragging] = useState(false);

  const finish = useCallback((commit: boolean) => {
    const active = activeRef.current;
    if (active === null) return;
    activeRef.current = null;
    const opts = optionsRef.current;
    const scheduler = opts.scheduler ?? defaultScheduler;
    if (active.frame !== null) scheduler.cancel(active.frame);
    active.cleanup();
    if (active.target.hasPointerCapture?.(active.pointerId) === true) {
      active.target.releasePointerCapture(active.pointerId);
    }
    clearInline(opts.windowRef.current);
    gestureSignal.end();
    setSnapPreview(null);
    setDragging(false);
    if (commit && active.started) opts.onCommit({ rect: active.current, snap: active.snap });
  }, []);

  // Desmontar no meio do gesto não deixa o sinal global preso.
  useEffect(() => () => finish(false), [finish]);

  const begin = useCallback(
    (mode: GestureMode, event: ReactPointerEvent<HTMLElement>) => {
      const opts = optionsRef.current;
      if (opts.enabled === false || activeRef.current !== null) return;
      if (event.button !== 0 || !event.isPrimary) return;
      const target = event.currentTarget;
      if (mode === "move" && event.target instanceof Element && event.target.closest(NO_DRAG_SELECTOR) !== null) return;
      event.preventDefault();
      target.setPointerCapture?.(event.pointerId);

      const onMove = (e: PointerEvent): void => {
        const active = activeRef.current;
        if (active === null || e.pointerId !== active.pointerId) return;
        const o = optionsRef.current;
        const dx = e.clientX - active.startX;
        const dy = e.clientY - active.startY;
        if (!active.started && dx === 0 && dy === 0) return;
        if (!active.started) {
          active.started = true;
          o.onStart?.(active.mode);
        }
        active.current = gestureRect(active.start, active.mode, dx, dy, o.workspace);
        if (active.mode === "move") {
          const origin = o.workspaceOrigin?.() ?? { x: 0, y: 0 };
          const snap = snapZoneAt({ x: e.clientX - origin.x, y: e.clientY - origin.y }, o.workspace);
          if (snap !== active.snap) {
            active.snap = snap;
            setSnapPreview(snap);
          }
        }
        if (active.frame === null) {
          const scheduler = o.scheduler ?? defaultScheduler;
          active.frame = scheduler.request(() => {
            const current = activeRef.current;
            if (current === null) return;
            current.frame = null;
            const element = optionsRef.current.windowRef.current;
            if (element === null) return;
            if (current.mode === "move") {
              element.style.transform = `translate(${current.current.x - current.start.x}px, ${current.current.y - current.start.y}px)`;
            } else {
              element.style.width = `${current.current.w}px`;
              element.style.height = `${current.current.h}px`;
            }
          });
        }
      };
      const onUp = (e: PointerEvent): void => {
        if (activeRef.current?.pointerId === e.pointerId) finish(true);
      };
      const onCancel = (e: PointerEvent): void => {
        if (activeRef.current?.pointerId === e.pointerId) finish(false);
      };
      target.addEventListener("pointermove", onMove);
      target.addEventListener("pointerup", onUp);
      target.addEventListener("pointercancel", onCancel);
      target.addEventListener("lostpointercapture", onUp);

      activeRef.current = {
        mode,
        pointerId: event.pointerId,
        startX: event.clientX,
        startY: event.clientY,
        start: { ...opts.rect },
        current: { ...opts.rect },
        snap: null,
        frame: null,
        target,
        started: false,
        cleanup: () => {
          target.removeEventListener("pointermove", onMove);
          target.removeEventListener("pointerup", onUp);
          target.removeEventListener("pointercancel", onCancel);
          target.removeEventListener("lostpointercapture", onUp);
        },
      };
      gestureSignal.begin();
      setDragging(true);
    },
    [finish],
  );

  const onMovePointerDown = useCallback((event: ReactPointerEvent<HTMLElement>) => begin("move", event), [begin]);
  const onResizePointerDown = useCallback(
    (mode: Exclude<GestureMode, "move">) => (event: ReactPointerEvent<HTMLElement>) => begin(mode, event),
    [begin],
  );

  return { onMovePointerDown, onResizePointerDown, snapPreview, dragging };
}

/** Há gesto de janela em curso: os iframes devem ficar com `pointer-events: none`. */
export function useGestureActive(): boolean {
  return useSyncExternalStore(gestureSignal.subscribe, gestureSignal.active, () => false);
}
