/*
 * Menu da janela, a alternativa de ponteiro único (WCAG 2.5.7): encaixar, tamanho, mover e
 * redimensionar por teclado, maximizar, minimizar e fechar. Comandos puros, que viram ações
 * do reducer; o modo teclado é uma pequena máquina de estados (setas, Enter, Esc).
 */
import { t, type MessageKey } from "../i18n";
import {
  clampRect,
  MIN_WINDOW_H,
  MIN_WINDOW_W,
  type Rect,
  type SizePreset,
  type SnapTarget,
  type WindowAction,
  type WindowKind,
  type Workspace,
} from "./windows";

/** Passo das setas no modo teclado. */
export const KEYBOARD_STEP = 16;

type WindowMenuCommand =
  | { type: "snap"; target: Exclude<SnapTarget, "top"> }
  | { type: "size"; size: SizePreset }
  | { type: "keyboardMove" }
  | { type: "keyboardResize" }
  | { type: "toggleMaximize" }
  | { type: "minimize" }
  | { type: "close" };

export interface WindowMenuItem {
  id: string;
  /** No idioma ativo (spec 101). */
  readonly label: string;
  group: "snap" | "size" | "keyboard" | "window";
  command: WindowMenuCommand;
}

/** Item do menu com o rótulo lido no idioma ativo. */
function menuItem(id: string, label: MessageKey, group: WindowMenuItem["group"], command: WindowMenuCommand): WindowMenuItem {
  return {
    id,
    get label() {
      return t(label);
    },
    group,
    command,
  };
}

export const WINDOW_MENU: readonly WindowMenuItem[] = [
  menuItem("snap-left", "windowMenu.snapLeft", "snap", { type: "snap", target: "left" }),
  menuItem("snap-right", "windowMenu.snapRight", "snap", { type: "snap", target: "right" }),
  menuItem("snap-top-left", "windowMenu.snapTopLeft", "snap", { type: "snap", target: "top-left" }),
  menuItem("snap-top-right", "windowMenu.snapTopRight", "snap", { type: "snap", target: "top-right" }),
  menuItem("snap-bottom-left", "windowMenu.snapBottomLeft", "snap", { type: "snap", target: "bottom-left" }),
  menuItem("snap-bottom-right", "windowMenu.snapBottomRight", "snap", { type: "snap", target: "bottom-right" }),
  menuItem("snap-center", "windowMenu.center", "snap", { type: "snap", target: "center" }),
  menuItem("size-compact", "windowMenu.sizeCompact", "size", { type: "size", size: "compact" }),
  menuItem("size-medium", "windowMenu.sizeMedium", "size", { type: "size", size: "medium" }),
  menuItem("size-wide", "windowMenu.sizeWide", "size", { type: "size", size: "wide" }),
  menuItem("keyboard-move", "windowMenu.keyboardMove", "keyboard", { type: "keyboardMove" }),
  menuItem("keyboard-resize", "windowMenu.keyboardResize", "keyboard", { type: "keyboardResize" }),
  menuItem("maximize", "windowControls.maximize", "window", { type: "toggleMaximize" }),
  menuItem("minimize", "windowControls.minimize", "window", { type: "minimize" }),
  menuItem("close", "windowControls.close", "window", { type: "close" }),
];

/** Ação do reducer de um comando do menu; os modos de teclado não são ação (`null`). */
export function menuCommandAction(kind: WindowKind, command: WindowMenuCommand): WindowAction | null {
  switch (command.type) {
    case "snap":
      return { type: "snap", kind, target: command.target };
    case "size":
      return { type: "size", kind, size: command.size };
    case "toggleMaximize":
      return { type: "toggleMaximize", kind };
    case "minimize":
      return { type: "minimize", kind };
    case "close":
      return { type: "close", kind };
    case "keyboardMove":
    case "keyboardResize":
      return null;
  }
}

/** Modo mover/redimensionar por teclado em curso (prévia local; o estado só muda no Enter). */
export interface KeyboardGesture {
  kind: WindowKind;
  mode: "move" | "resize";
  /** Geometria antes do modo (o Esc volta a ela). */
  origin: Rect;
  rect: Rect;
}

export function startKeyboardGesture(kind: WindowKind, mode: "move" | "resize", rect: Rect): KeyboardGesture {
  return { kind, mode, origin: { ...rect }, rect: { ...rect } };
}

type KeyboardGestureResult =
  | { type: "update"; gesture: KeyboardGesture }
  | { type: "commit"; action: WindowAction }
  | { type: "cancel"; rect: Rect };

const ARROWS: Readonly<Record<string, [number, number]>> = {
  ArrowLeft: [-1, 0],
  ArrowRight: [1, 0],
  ArrowUp: [0, -1],
  ArrowDown: [0, 1],
};

/** Uma tecla no modo teclado; teclas sem efeito devolvem `null` (não consumir o evento). */
export function keyboardGestureStep(gesture: KeyboardGesture, key: string, ws: Workspace): KeyboardGestureResult | null {
  if (key === "Enter") return { type: "commit", action: { type: "setRect", kind: gesture.kind, rect: gesture.rect } };
  if (key === "Escape") return { type: "cancel", rect: gesture.origin };
  const delta = ARROWS[key];
  if (delta === undefined) return null;
  const [dx, dy] = delta;
  const r = gesture.rect;
  let next: Rect;
  if (gesture.mode === "move") {
    next = clampRect({ ...r, x: r.x + dx * KEYBOARD_STEP, y: r.y + dy * KEYBOARD_STEP }, ws);
  } else {
    const w = Math.max(Math.min(MIN_WINDOW_W, ws.w), Math.min(ws.w - r.x, r.w + dx * KEYBOARD_STEP));
    const h = Math.max(Math.min(MIN_WINDOW_H, ws.h), Math.min(ws.h - r.y, r.h + dy * KEYBOARD_STEP));
    next = clampRect({ ...r, w, h }, ws);
  }
  return { type: "update", gesture: { ...gesture, rect: next } };
}
