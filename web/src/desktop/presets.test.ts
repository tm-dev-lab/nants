import { describe, expect, it } from "vitest";
import type { ProjectPhase } from "../store";
import {
  categoryOf,
  emptyCategoryLayouts,
  layoutForCategory,
  LAYOUT_CATEGORIES,
  mineLayoutFor,
  onPhaseChange,
  parseCategoryLayouts,
  presetLayout,
  presetWindows,
  restoreDefault,
  saveCurrent,
  saveMine,
} from "./presets";
import { shownByZ, type Layout, type Workspace } from "./windows";

const WS: Workspace = { w: 1440, h: 800 };
const custom: Layout = { windows: { files: { x: 10, y: 10, w: 400, h: 300, z: 1, mode: "normal", open: true } } };

describe("categoryOf", () => {
  it("cobre as 9 fases (decisão 5; a 094 acrescenta reviewing_plan)", () => {
    const expected: Record<ProjectPhase, string> = {
      idle: "planning",
      planning: "planning",
      waiting_approval: "planning",
      reviewing_plan: "planning",
      executing: "execution",
      finalizing: "execution",
      blocked: "execution",
      stalled: "execution",
      integrated: "done",
    };
    for (const [phase, category] of Object.entries(expected)) {
      expect(categoryOf(phase as ProjectPhase)).toBe(category);
    }
  });
});

describe("presets por categoria", () => {
  it("janelas de cada preset", () => {
    expect(new Set(presetWindows("planning"))).toEqual(new Set(["conversation", "plan"]));
    expect(new Set(presetWindows("execution"))).toEqual(new Set(["conversation", "board", "activity", "files"]));
    expect(new Set(presetWindows("done"))).toEqual(new Set(["report", "app", "conversation", "execution"]));
  });

  it("concluído (095/T03, C-05): Relatório à esquerda, App em cima, Conversa e Execução embaixo", () => {
    const ws = { w: 2000, h: 1000 };
    const layout = presetLayout("done", ws);
    const at = (kind: "report" | "app" | "conversation" | "execution") => layout.windows[kind];
    // Frações: Relatório {0, 0, 0.35, 1}, App {0.35, 0, 0.65, 0.6}, Conversa {0.35, 0.6, 0.325, 0.4},
    // Execução {0.675, 0.6, 0.325, 0.4}, com a folga de 12 px nas bordas e 6 px de cada lado entre janelas.
    expect(at("report")).toMatchObject({ x: 12, y: 12, w: 700 - 6 - 12, h: 1000 - 24 });
    expect(at("app")).toMatchObject({ x: 706, y: 12, w: 2000 - 12 - 706, h: 600 - 6 - 12 });
    expect(at("conversation")).toMatchObject({ x: 706, y: 606, w: 1350 - 6 - 706, h: 1000 - 12 - 606 });
    expect(at("execution")).toMatchObject({ x: 1356, y: 606, w: 2000 - 12 - 1356, h: 1000 - 12 - 606 });
  });

  it("concluído: mínimo de 320×220 em cada janela na menor área do desktop (1024×640)", () => {
    const ws = { w: 1024, h: 640 };
    const layout = presetLayout("done", ws);
    for (const kind of presetWindows("done")) {
      const win = layout.windows[kind];
      expect({ kind, w: (win?.w ?? 0) >= 320, h: (win?.h ?? 0) >= 220 }).toEqual({ kind, w: true, h: true });
      expect((win?.x ?? 0) + (win?.w ?? 0)).toBeLessThanOrEqual(ws.w);
      expect((win?.y ?? 0) + (win?.h ?? 0)).toBeLessThanOrEqual(ws.h);
    }
  });

  it("arranjo salvo do concluído sem a Execução (antes da 095) continua válido e não migra", () => {
    const saved = {
      done: {
        current: {
          windows: {
            report: { x: 12, y: 12, w: 400, h: 600, z: 1, mode: "normal", open: true },
            app: { x: 420, y: 12, w: 500, h: 400, z: 2, mode: "normal", open: true },
          },
        },
        mine: null,
      },
    };
    const layouts = parseCategoryLayouts(saved);
    expect(layouts.done.current).not.toBeNull();
    const layout = layoutForCategory(layouts, "done", WS);
    expect(Object.keys(layout.windows).sort()).toEqual(["app", "report"]);
    expect(layout.windows.execution).toBeUndefined();
    // "Restaurar o padrão" traz o preset novo, com a Execução.
    expect(restoreDefault(layouts, "done", WS).layout.windows.execution?.open).toBe(true);
  });

  it("escalados à área: dentro dela, sem sobreposição e todas visíveis", () => {
    for (const ws of [WS, { w: 1024, h: 640 }, { w: 2560, h: 1300 }]) {
      for (const category of LAYOUT_CATEGORIES) {
        const layout = presetLayout(category, ws);
        const wins = Object.values(layout.windows);
        expect(shownByZ(layout)).toHaveLength(presetWindows(category).length);
        for (const w of wins) {
          expect(w.x).toBeGreaterThanOrEqual(0);
          expect(w.y).toBeGreaterThanOrEqual(0);
          expect(w.x + w.w).toBeLessThanOrEqual(ws.w);
          expect(w.y + w.h).toBeLessThanOrEqual(ws.h);
        }
        for (const a of wins)
          for (const b of wins) {
            if (a === b) continue;
            const overlap = a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
            expect(overlap).toBe(false);
          }
      }
    }
  });

  it("planejamento na área de 1440×800: Conversa à esquerda e Plano à direita", () => {
    const layout = presetLayout("planning", WS);
    expect(layout.windows.conversation?.x).toBe(12);
    expect((layout.windows.plan?.x ?? 0) + (layout.windows.plan?.w ?? 0)).toBe(WS.w - 12);
  });
});

describe("current / mine", () => {
  it("aplicar o meu layout (sem ele, o padrão) e salvar", () => {
    const layouts = emptyCategoryLayouts();
    expect(mineLayoutFor(layouts, "execution", WS)).toEqual(presetLayout("execution", WS));
    const saved = saveMine(layouts, "execution", custom);
    expect(mineLayoutFor(saved, "execution", WS)).toEqual(custom);
    expect(saved.planning).toEqual({ current: null, mine: null });
  });

  it("ao entrar na categoria: atual salvo > meu > padrão", () => {
    let layouts = emptyCategoryLayouts();
    expect(layoutForCategory(layouts, "planning", WS)).toEqual(presetLayout("planning", WS));
    layouts = saveMine(layouts, "planning", custom);
    expect(layoutForCategory(layouts, "planning", WS)).toEqual(custom);
    const current: Layout = { windows: { plan: { x: 0, y: 0, w: 500, h: 500, z: 1, mode: "maximized", open: true } } };
    layouts = saveCurrent(layouts, "planning", current);
    expect(layoutForCategory(layouts, "planning", WS)).toEqual(current);
  });

  it("restaurar o padrão esquece o meu layout e devolve o preset", () => {
    const layouts = saveMine(emptyCategoryLayouts(), "done", custom);
    const { layouts: next, layout } = restoreDefault(layouts, "done", WS);
    expect(layout).toEqual(presetLayout("done", WS));
    expect(next.done.mine).toBeNull();
    expect(next.done.current).toEqual(layout);
  });

  it("estado salvo inválido volta ao padrão sem erro", () => {
    const parsed = parseCategoryLayouts({
      planning: { current: { windows: { conversation: { x: "ruim" } } }, mine: custom },
      execution: "lixo",
      __proto__: { polluted: true },
    });
    expect(parsed.planning.current).toBeNull();
    expect(parsed.planning.mine).toEqual(custom);
    expect(parsed.execution).toEqual({ current: null, mine: null });
    expect(parseCategoryLayouts(null)).toEqual(emptyCategoryLayouts());
    expect(parseCategoryLayouts([1, 2])).toEqual(emptyCategoryLayouts());
  });
});

describe("onPhaseChange", () => {
  const state = { layouts: emptyCategoryLayouts(), layout: custom, workspace: WS };

  it("mesma categoria não troca o layout", () => {
    expect(onPhaseChange("planning", "waiting_approval", state)).toBeNull();
    expect(onPhaseChange("executing", "blocked", state)).toBeNull();
    expect(onPhaseChange("stalled", "finalizing", state)).toBeNull();
  });

  it("categoria nova: guarda o atual na anterior e aplica o da nova", () => {
    const change = onPhaseChange("waiting_approval", "executing", state);
    expect(change?.category).toBe("execution");
    expect(change?.layouts.planning.current).toEqual(custom);
    expect(change?.layout).toEqual(presetLayout("execution", WS));
    // Voltar ao planejamento (spec nova depois da conclusão) restaura o arranjo salvo.
    const after = onPhaseChange("integrated", "planning", { ...state, layouts: change?.layouts ?? emptyCategoryLayouts(), layout: presetLayout("done", WS) });
    expect(after?.category).toBe("planning");
    expect(after?.layout).toEqual(custom);
    expect(after?.layouts.done.current).toEqual(presetLayout("done", WS));
  });

  it("primeira carga (prev null) aplica o da categoria sem salvar nada", () => {
    const change = onPhaseChange(null, "integrated", state);
    expect(change?.category).toBe("done");
    expect(change?.layouts).toEqual(state.layouts);
    expect(change?.layout).toEqual(presetLayout("done", WS));
  });
});
