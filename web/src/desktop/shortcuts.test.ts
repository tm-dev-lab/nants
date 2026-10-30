import { describe, expect, it } from "vitest";
import { setBody } from "./test-utils";
import {
  appSlotAria,
  matchShortcut,
  shortcutById,
  shortcutContext,
  SHORTCUTS,
  type KeyLike,
  type ShortcutContext,
  type ShortcutId,
} from "./shortcuts";
import { KEYBOARD_STEP, keyboardGestureStep, menuCommandAction, startKeyboardGesture, WINDOW_MENU } from "./windowMenu";

const key = (k: string, mods: Partial<KeyLike> = {}): KeyLike => ({
  key: k,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});
const desktop: ShortcutContext = { typing: false, onDesktop: true, onTitleBar: false };
const typing: ShortcutContext = { typing: true, onDesktop: false, onTitleBar: false };

describe("tabela única de atalhos", () => {
  it("tem todos os atalhos da spec, sem Alt+Espaço nem Ctrl+`", () => {
    const ids: ShortcutId[] = ["launcher", "nextWindow", "previousWindow", "nMenu", "appSlot", "shortcutSheet", "undoLayout", "windowMenu"];
    for (const id of ids) expect(shortcutById(id)?.id).toBe(id);
    const all = SHORTCUTS.map((s) => `${s.aria} ${s.keys}`).join(" ");
    expect(all).not.toMatch(/Alt\+Space|Alt\+Espaço|Control\+`|Ctrl\+`/);
    // Menu do N (spec 137): Alt+0, no lugar do Alt+Espaço do mockup.
    expect(shortcutById("nMenu")).toMatchObject({ aria: "Alt+0", keys: "Alt+0" });
    expect(SHORTCUTS.filter((s) => s.allowWhileTyping).map((s) => s.id)).toEqual(["launcher"]);
  });

  it("aria-keyshortcuts dos apps na grade do menu do N", () => {
    expect(appSlotAria(1)).toBe("Alt+1");
    expect(appSlotAria(9)).toBe("Alt+9");
    expect(appSlotAria(10)).toBeUndefined();
  });
});

describe("matchShortcut", () => {
  it("Ctrl+K (e Cmd+K) abre o lançador até dentro de campos", () => {
    expect(matchShortcut(key("k", { ctrlKey: true }), desktop)).toEqual({ id: "launcher" });
    expect(matchShortcut(key("K", { metaKey: true }), typing)).toEqual({ id: "launcher" });
  });

  it("F6 / Shift+F6", () => {
    expect(matchShortcut(key("F6"), desktop)).toEqual({ id: "nextWindow" });
    expect(matchShortcut(key("F6", { shiftKey: true }), desktop)).toEqual({ id: "previousWindow" });
  });

  it("Alt+1…9 pelo código físico (layouts que trocam o key com Alt)", () => {
    expect(matchShortcut(key("1", { altKey: true, code: "Digit1" }), desktop)).toEqual({ id: "appSlot", slot: 1 });
    expect(matchShortcut(key("¡", { altKey: true, code: "Digit9" }), desktop)).toEqual({ id: "appSlot", slot: 9 });
    expect(matchShortcut(key("0", { altKey: true, code: "Digit0" }), desktop)).toEqual({ id: "nMenu" });
  });

  it("Alt+0 abre o menu do N pelo código físico Digit0; Alt+Espaço não é atalho", () => {
    expect(matchShortcut(key("º", { altKey: true, code: "Digit0" }), desktop)).toEqual({ id: "nMenu" });
    expect(matchShortcut(key("0", { altKey: true }), desktop)).toEqual({ id: "nMenu" });
    expect(matchShortcut(key("0", { altKey: true, code: "Numpad0" }), desktop)).toBeNull();
    expect(matchShortcut(key("0", { code: "Digit0" }), desktop)).toBeNull();
    expect(matchShortcut(key("0", { altKey: true, ctrlKey: true, code: "Digit0" }), desktop)).toBeNull();
    expect(matchShortcut(key(" ", { altKey: true, code: "Space" }), desktop)).toBeNull();
    expect(matchShortcut(key("0", { altKey: true, code: "Digit0" }), typing)).toBeNull();
  });

  it("Shift+/ abre a folha de atalhos", () => {
    expect(matchShortcut(key("?", { shiftKey: true }), desktop)).toEqual({ id: "shortcutSheet" });
    expect(matchShortcut(key("/", { shiftKey: true, code: "Slash" }), desktop)).toEqual({ id: "shortcutSheet" });
    expect(matchShortcut(key("/"), desktop)).toBeNull();
  });

  it("Ctrl+Z só com foco no desktop", () => {
    expect(matchShortcut(key("z", { ctrlKey: true }), desktop)).toEqual({ id: "undoLayout" });
    expect(matchShortcut(key("z", { ctrlKey: true }), { ...desktop, onDesktop: false })).toBeNull();
  });

  it("Shift+F10 e tecla Menu só com foco na barra de título", () => {
    const bar = { ...desktop, onTitleBar: true };
    expect(matchShortcut(key("F10", { shiftKey: true }), bar)).toEqual({ id: "windowMenu" });
    expect(matchShortcut(key("ContextMenu"), bar)).toEqual({ id: "windowMenu" });
    expect(matchShortcut(key("F10", { shiftKey: true }), desktop)).toBeNull();
  });

  it("com foco em campo de texto, só o Ctrl+K dispara", () => {
    for (const k of [key("F6"), key("1", { altKey: true, code: "Digit1" }), key("?", { shiftKey: true }), key("z", { ctrlKey: true })]) {
      expect(matchShortcut(k, typing)).toBeNull();
    }
  });
});

describe("shortcutContext + isTypingTarget", () => {
  it("campos de texto contam como digitação; barra e conteúdo da janela são reconhecidos", () => {
    setBody(`
      <div data-window-titlebar><button id="t">Conversa</button></div>
      <div data-window-content><input id="i" /><button id="b">ok</button></div>
      <textarea id="ta"></textarea><div id="ce" contenteditable="true"></div>`);
    const el = (id: string): Element | null => document.getElementById(id);
    expect(shortcutContext(el("i"))).toEqual({ typing: true, onDesktop: false, onTitleBar: false });
    expect(shortcutContext(el("ta")).typing).toBe(true);
    expect(shortcutContext(el("ce")).typing).toBe(true);
    expect(shortcutContext(el("t"))).toEqual({ typing: false, onDesktop: true, onTitleBar: true });
    expect(shortcutContext(el("b"))).toEqual({ typing: false, onDesktop: false, onTitleBar: false });
    expect(shortcutContext(document.body)).toEqual({ typing: false, onDesktop: true, onTitleBar: false });
    expect(shortcutContext(null).typing).toBe(false);
  });
});

describe("menu da janela", () => {
  it("itens: encaixes, tamanhos, teclado, maximizar, minimizar e fechar", () => {
    expect(WINDOW_MENU.map((i) => i.id)).toEqual([
      "snap-left",
      "snap-right",
      "snap-top-left",
      "snap-top-right",
      "snap-bottom-left",
      "snap-bottom-right",
      "snap-center",
      "size-compact",
      "size-medium",
      "size-wide",
      "keyboard-move",
      "keyboard-resize",
      "maximize",
      "minimize",
      "close",
    ]);
    expect(menuCommandAction("plan", { type: "snap", target: "left" })).toEqual({ type: "snap", kind: "plan", target: "left" });
    expect(menuCommandAction("plan", { type: "size", size: "wide" })).toEqual({ type: "size", kind: "plan", size: "wide" });
    expect(menuCommandAction("plan", { type: "close" })).toEqual({ type: "close", kind: "plan" });
    expect(menuCommandAction("plan", { type: "keyboardMove" })).toBeNull();
  });

  const ws = { w: 1000, h: 700 };
  const rect = { x: 100, y: 100, w: 400, h: 300 };

  it("mover por setas de 16 px; Enter confirma", () => {
    let g = startKeyboardGesture("plan", "move", rect);
    for (const k of ["ArrowRight", "ArrowRight", "ArrowDown"]) {
      const r = keyboardGestureStep(g, k, ws);
      if (r?.type !== "update") throw new Error("esperava update");
      g = r.gesture;
    }
    expect(g.rect).toEqual({ x: 100 + 2 * KEYBOARD_STEP, y: 100 + KEYBOARD_STEP, w: 400, h: 300 });
    expect(keyboardGestureStep(g, "Enter", ws)).toEqual({ type: "commit", action: { type: "setRect", kind: "plan", rect: g.rect } });
  });

  it("redimensionar por setas respeita o mínimo; Esc cancela e restaura", () => {
    let g = startKeyboardGesture("plan", "resize", { ...rect, w: 330, h: 230 });
    for (const k of ["ArrowLeft", "ArrowUp"]) {
      const r = keyboardGestureStep(g, k, ws);
      if (r?.type !== "update") throw new Error("esperava update");
      g = r.gesture;
    }
    expect(g.rect).toMatchObject({ w: 320, h: 220 });
    expect(keyboardGestureStep(g, "Escape", ws)).toEqual({ type: "cancel", rect: { ...rect, w: 330, h: 230 } });
    expect(keyboardGestureStep(g, "a", ws)).toBeNull();
  });

  it("mover por teclado não sai da área", () => {
    let g = startKeyboardGesture("plan", "move", { x: 0, y: 0, w: 400, h: 300 });
    const r = keyboardGestureStep(g, "ArrowLeft", ws);
    if (r?.type !== "update") throw new Error("esperava update");
    g = r.gesture;
    expect(g.rect.x).toBe(0);
  });
});
