import { afterEach, describe, expect, it } from "vitest";
import { setBody } from "./test-utils";
import {
  applyFocus,
  createFocusMemory,
  cycleWindow,
  focusAfterHide,
  focusAfterRestore,
  N_BUTTON_ID,
  QUIET_FOCUS_ATTR,
  windowHiddenAttrs,
  windowTitleBarId,
} from "./focus";
import type { Layout, WindowState } from "./windows";

const win = (patch: Partial<WindowState> = {}): WindowState => ({ x: 0, y: 0, w: 400, h: 300, z: 1, mode: "normal", open: true, ...patch });

afterEach(() => {
  document.body.replaceChildren();
});

describe("regras de foco", () => {
  it("minimizar ou fechar → o botão N da barra de cima (137/T02: sem dock)", () => {
    setBody(`<header><button id="${N_BUTTON_ID}">Menu do Nants</button></header>`);
    const target = focusAfterHide();
    expect(target).toEqual({ type: "nButton" });
    expect(applyFocus(target)).toBe(true);
    expect(document.activeElement?.id).toBe(N_BUTTON_ID);
  });

  it("restaurar → último elemento focado na janela; se sumiu, a barra de título", () => {
    setBody(`<div id="${windowTitleBarId("plan")}" tabindex="-1"></div><input id="campo" />`);
    const memory = createFocusMemory();
    const field = document.getElementById("campo") as HTMLElement;
    memory.remember("plan", field);
    const target = focusAfterRestore("plan", memory);
    expect(target).toEqual({ type: "element", element: field });
    expect(applyFocus(target)).toBe(true);
    expect(document.activeElement).toBe(field);

    field.remove();
    expect(focusAfterRestore("plan", memory)).toEqual({ type: "titleBar", kind: "plan" });
    memory.forget("plan");
    const fallback = focusAfterRestore("plan", memory);
    expect(applyFocus(fallback)).toBe(true);
    expect(document.activeElement?.id).toBe("window-plan-titlebar");
  });

  it("alvo ausente no documento não lança", () => {
    expect(applyFocus({ type: "nButton" })).toBe(false);
    expect(applyFocus({ type: "titleBar", kind: "files" })).toBe(false);
  });

  it("minimizada ou fechada = hidden + inert; visível não", () => {
    expect(windowHiddenAttrs(win({ mode: "minimized" }))).toEqual({ hidden: true, inert: true });
    expect(windowHiddenAttrs(win({ open: false }))).toEqual({ hidden: true, inert: true });
    expect(windowHiddenAttrs(undefined)).toEqual({ hidden: true, inert: true });
    expect(windowHiddenAttrs(win({ mode: "maximized" }))).toEqual({ hidden: false, inert: false });
  });
});

describe("foco programático sem anel na barra de título (095/T07, decisão 2)", () => {
  it("sem `ring`: marca a barra como foco silencioso até a primeira tecla", () => {
    setBody(`<div id="${windowTitleBarId("plan")}" tabindex="0"></div>`);
    expect(applyFocus({ type: "titleBar", kind: "plan" })).toBe(true);
    const bar = document.getElementById(windowTitleBarId("plan"));
    expect(bar?.getAttribute(QUIET_FOCUS_ATTR)).toBe("true");
    bar?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    expect(bar?.hasAttribute(QUIET_FOCUS_ATTR)).toBe(false);
  });

  it("sair do foco também remove a marca; com `ring` (F6, Esc) não há marca", () => {
    setBody(`<div id="${windowTitleBarId("plan")}" tabindex="0"></div><div id="${windowTitleBarId("files")}" tabindex="0"></div>`);
    applyFocus({ type: "titleBar", kind: "plan" });
    const plan = document.getElementById(windowTitleBarId("plan"));
    expect(applyFocus({ type: "titleBar", kind: "files", ring: true })).toBe(true);
    expect(plan?.hasAttribute(QUIET_FOCUS_ATTR)).toBe(false);
    expect(document.getElementById(windowTitleBarId("files"))?.hasAttribute(QUIET_FOCUS_ATTR)).toBe(false);
  });

  it("o botão N e os elementos lembrados nunca recebem a marca", () => {
    setBody(`<button id="${N_BUTTON_ID}">Menu do Nants</button>`);
    applyFocus({ type: "nButton" });
    expect(document.getElementById(N_BUTTON_ID)?.hasAttribute(QUIET_FOCUS_ATTR)).toBe(false);
  });
});

describe("cycleWindow (F6 / Shift+F6)", () => {
  const layout: Layout = {
    windows: {
      conversation: win({ z: 3 }),
      plan: win({ z: 1, mode: "minimized" }),
      board: win({ z: 2 }),
      files: win({ z: 4 }),
    },
  };

  it("percorre só as visíveis na ordem dos apps, com volta", () => {
    expect(cycleWindow(layout, "conversation", 1)).toBe("board");
    expect(cycleWindow(layout, "board", 1)).toBe("files");
    expect(cycleWindow(layout, "files", 1)).toBe("conversation");
    expect(cycleWindow(layout, "conversation", -1)).toBe("files");
    expect(cycleWindow(layout, null, 1)).toBe("conversation");
    expect(cycleWindow(layout, null, -1)).toBe("files");
    expect(cycleWindow({ windows: {} }, null, 1)).toBeNull();
  });
});
