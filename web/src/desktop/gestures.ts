/*
 * Regras puras dos gestos de janela (spec 093, decisão 4): geometria durante o arraste,
 * zona de encaixe a 16 px da borda e o sinal global "há gesto em curso" (para desligar
 * `pointer-events` nos iframes, que senão engolem o `pointermove`).
 */
import { localized, t } from "../i18n";
import { clampRect, MIN_WINDOW_H, MIN_WINDOW_W, snapRect, type Rect, type SnapTarget, type Workspace } from "./windows";

/** Distância da borda que ativa a prévia de encaixe. */
export const SNAP_THRESHOLD = 16;

export type GestureMode = "move" | "resize-e" | "resize-s" | "resize-se";

/** Geometria durante o gesto, já limitada à área e aos mínimos. */
export function gestureRect(start: Rect, mode: GestureMode, dx: number, dy: number, ws: Workspace): Rect {
  if (mode === "move") return clampRect({ ...start, x: start.x + dx, y: start.y + dy }, ws);
  const minW = Math.min(MIN_WINDOW_W, ws.w);
  const minH = Math.min(MIN_WINDOW_H, ws.h);
  const w = mode === "resize-s" ? start.w : Math.max(minW, Math.min(ws.w - start.x, start.w + dx));
  const h = mode === "resize-e" ? start.h : Math.max(minH, Math.min(ws.h - start.y, start.h + dy));
  return clampRect({ ...start, w, h }, ws);
}

/**
 * Encaixe pela posição do ponteiro (coordenadas da área de trabalho): perto de duas bordas
 * → o quarto; da esquerda/direita → a metade; do topo → maximizar. Fora → `null`.
 */
export function snapZoneAt(point: { x: number; y: number }, ws: Workspace, threshold = SNAP_THRESHOLD): SnapTarget | null {
  const left = point.x <= threshold;
  const right = point.x >= ws.w - threshold;
  const top = point.y <= threshold;
  const bottom = point.y >= ws.h - threshold;
  if (top && left) return "top-left";
  if (top && right) return "top-right";
  if (bottom && left) return "bottom-left";
  if (bottom && right) return "bottom-right";
  if (left) return "left";
  if (right) return "right";
  if (top) return "top";
  return null;
}

/** Retângulo da prévia translúcida de um encaixe. */
export function snapPreviewRect(target: SnapTarget, current: Rect, ws: Workspace): Rect {
  return snapRect(target, current, ws);
}

const SNAP_LABELS: Readonly<Record<SnapTarget, string>> = localized({
  left: "announce.snap.left",
  right: "announce.snap.right",
  "top-left": "announce.snap.topLeft",
  "top-right": "announce.snap.topRight",
  "bottom-left": "announce.snap.bottomLeft",
  "bottom-right": "announce.snap.bottomRight",
  center: "announce.snap.center",
  top: "announce.snap.top",
});

/** Texto do anúncio (`role="status"`) depois de um encaixe. */
export function snapAnnouncement(title: string, target: SnapTarget): string {
  return t("announce.snapped", { title, target: SNAP_LABELS[target] });
}

// --- Sinal global de gesto (useSyncExternalStore) ---

let gestureCount = 0;
const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

export const gestureSignal = {
  begin(): void {
    gestureCount += 1;
    if (gestureCount === 1) emit();
  },
  end(): void {
    if (gestureCount === 0) return;
    gestureCount -= 1;
    if (gestureCount === 0) emit();
  },
  active(): boolean {
    return gestureCount > 0;
  },
  subscribe(listener: () => void): () => void {
    listeners.add(listener);
    return () => {
      listeners.delete(listener);
    };
  },
  /** Só para testes. */
  reset(): void {
    gestureCount = 0;
    emit();
  },
};
