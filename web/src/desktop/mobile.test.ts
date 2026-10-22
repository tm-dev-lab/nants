/* Modo celular (spec 093, T10; decisão 17): modelo puro do alternador e limite de largura. */
import { describe, expect, it } from "vitest";
import {
  EMPTY_MOBILE_STATE,
  HOME_TABS,
  MOBILE_TABS,
  mergedSelection,
  mobileLayout,
  mobileViewFor,
  parseMobileState,
  serializeMobileState,
  viewWindow,
  withSelection,
  withView,
} from "./mobile";
import { DESKTOP_MIN_WIDTH, isMobileWidth, MOBILE_QUERY } from "./viewport";
import { isShown, topWindow, type Layout } from "./windows";

describe("limite do modo celular", () => {
  it("abaixo de 1024 px é celular; o iPad em paisagem (1024) fica no desktop", () => {
    expect(DESKTOP_MIN_WIDTH).toBe(1024);
    expect(isMobileWidth(390)).toBe(true);
    expect(isMobileWidth(768)).toBe(true);
    expect(isMobileWidth(1023)).toBe(true);
    expect(isMobileWidth(1024)).toBe(false);
    expect(isMobileWidth(1440)).toBe(false);
    expect(MOBILE_QUERY).toBe("(max-width: 1023.98px)");
  });
});

describe("abas por fase (decisão 17)", () => {
  it("planejamento, execução e concluído, sempre com o Alternador", () => {
    expect(MOBILE_TABS.planning).toEqual(["conversation", "plan", "switcher"]);
    expect(MOBILE_TABS.execution).toEqual(["conversation", "board", "switcher", "activity"]);
    // 095/T03 (C-12): a Execução vira aba só no Concluído; nas outras fases fica no Alternador.
    expect(MOBILE_TABS.done).toEqual(["report", "app", "execution", "switcher", "conversation"]);
    expect(MOBILE_TABS.planning).not.toContain("execution");
    expect(MOBILE_TABS.execution).not.toContain("execution");
    expect(HOME_TABS).toEqual(["home", "switcher"]);
  });

  it("a vista é a escolhida na mesma categoria; mudou a categoria, volta à primeira aba", () => {
    expect(mobileViewFor(EMPTY_MOBILE_STATE, "planning")).toBe("conversation");
    expect(mobileViewFor(EMPTY_MOBILE_STATE, "done")).toBe("report");
    const chosen = withView(EMPTY_MOBILE_STATE, "plan", "planning");
    expect(mobileViewFor(chosen, "planning")).toBe("plan");
    expect(mobileViewFor(chosen, "execution")).toBe("conversation");
    expect(mobileViewFor(withView(chosen, "switcher", "execution"), "execution")).toBe("switcher");
  });
});

describe("seção `mobile` do ui-state", () => {
  it("lê com segurança: valores inválidos viram o padrão; 'home' não vale num projeto", () => {
    expect(parseMobileState(undefined)).toEqual(EMPTY_MOBILE_STATE);
    expect(parseMobileState([1, 2])).toEqual(EMPTY_MOBILE_STATE);
    expect(parseMobileState({ view: "janela-x", category: "outra", selection: 3 })).toEqual(EMPTY_MOBILE_STATE);
    expect(parseMobileState({ view: "home", category: "planning" }).view).toBeNull();
    const parsed = parseMobileState({ view: "board", category: "execution", selection: { taskId: "T02", ["__proto__"]: { x: 1 } } });
    expect(parsed.view).toBe("board");
    expect(parsed.category).toBe("execution");
    expect(parsed.selection).toEqual({ taskId: "T02" });
  });

  it("vista e seleção do celular são gravadas juntas, sem perder uma à outra", () => {
    const a = withSelection(withView(EMPTY_MOBILE_STATE, "board", "execution"), { activityTab: "history" });
    const b = withView(a, "activity", "execution");
    expect(serializeMobileState(b)).toEqual({ view: "activity", category: "execution", selection: { activityTab: "history" } });
    expect(parseMobileState(serializeMobileState(b))).toEqual(b);
  });

  it("a seleção lida no celular é a do desktop com a do celular por cima (a do desktop não muda)", () => {
    const desktop = { taskId: "T01", filePath: "src/a.ts" };
    const merged = mergedSelection(desktop, { taskId: "T03" });
    expect(merged).toEqual({ taskId: "T03", filePath: "src/a.ts" });
    expect(desktop).toEqual({ taskId: "T01", filePath: "src/a.ts" });
    expect(mergedSelection(undefined, { taskId: "T03" })).toEqual({ taskId: "T03" });
  });
});

describe("arranjo derivado do celular", () => {
  const desktop: Layout = {
    windows: {
      conversation: { x: 10, y: 20, w: 500, h: 400, z: 3, mode: "normal", open: true },
      plan: { x: 520, y: 20, w: 500, h: 400, z: 4, mode: "normal", open: true },
      board: { x: 0, y: 0, w: 400, h: 300, z: 1, mode: "normal", open: false },
    },
  };
  const ws = { w: 390, h: 640 };

  it("uma janela por vez em tela cheia; as outras montadas ficam minimizadas (ocultas)", () => {
    const layout = mobileLayout(desktop, "plan", [], ws);
    expect(topWindow(layout)).toBe("plan");
    expect(layout.windows.plan).toMatchObject({ x: 0, y: 0, w: 390, h: 640, mode: "maximized", open: true });
    expect(isShown(layout.windows.conversation)).toBe(false);
    expect(layout.windows.conversation?.mode).toBe("minimized");
    // Fechada no desktop e nunca visitada no celular: não é montada.
    expect(layout.windows.board).toBeUndefined();
  });

  it("visitadas no celular continuam montadas; sem vista de janela, nenhuma aparece", () => {
    const layout = mobileLayout(desktop, null, ["board"], ws);
    expect(topWindow(layout)).toBeNull();
    expect(Object.keys(layout.windows).sort()).toEqual(["board", "conversation", "plan"]);
  });

  it("o arranjo do desktop não é alterado", () => {
    const before = JSON.stringify(desktop);
    mobileLayout(desktop, "conversation", ["files"], ws);
    expect(JSON.stringify(desktop)).toBe(before);
  });

  it("viewWindow: só janelas (o Alternador e o início não são janelas)", () => {
    expect(viewWindow("board")).toBe("board");
    expect(viewWindow("switcher")).toBeNull();
    expect(viewWindow("home")).toBeNull();
    expect(viewWindow(null)).toBeNull();
  });
});
