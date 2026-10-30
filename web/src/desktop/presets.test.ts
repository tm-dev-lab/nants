import { describe, expect, it } from "vitest";
import type { ProjectPhase } from "../store";
import {
  categoryOf,
  emptyCategoryLayouts,
  layoutForCategory,
  LAYOUT_CATEGORIES,
  migrateCategoryLayouts,
  migrateLayout,
  mineLayoutFor,
  onPhaseChange,
  parseCategoryLayouts,
  presetLayout,
  presetWindows,
  restoreDefault,
  saveCurrent,
  saveMine,
} from "./presets";
import { LEGACY_DOCK_RESERVE, workspaceForViewport } from "./metrics";
import { canUndo, createDesktopState, LAYOUT_VERSION, parseLayout, serializeLayout, shownByZ, windowsReducer, type Layout, type Workspace } from "./windows";

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

describe("sem dock (137/T02): área nova e migração única dos layouts salvos", () => {
  /** Viewport de 1440×860: com o dock, a área era de 732 px; sem ele, é de 816 px. */
  const NEW = workspaceForViewport(1440, 860);
  const OLD: Workspace = { w: NEW.w, h: NEW.h - LEGACY_DOCK_RESERVE };
  /** Um arranjo salvo antes da 137: o que o servidor devolve (JSON, sem versão). */
  const legacy = (layout: Layout): Layout => {
    const parsed = parseLayout(JSON.parse(JSON.stringify({ windows: serializeLayout(layout).windows })));
    if (parsed === null) throw new Error("arranjo inválido");
    return parsed;
  };

  it("os presets usam a altura nova: as janelas de baixo vão até a borda, com a folga das laterais", () => {
    expect(NEW).toEqual({ w: 1440, h: 816 });
    for (const category of LAYOUT_CATEGORIES) {
      const wins = Object.values(presetLayout(category, NEW).windows);
      const bottom = Math.max(...wins.map((w) => w.y + w.h));
      const right = Math.max(...wins.map((w) => w.x + w.w));
      expect({ category, bottomGap: NEW.h - bottom, rightGap: NEW.w - right }).toEqual({ category, bottomGap: 12, rightGap: 12 });
    }
  });

  it("sem versão: a janela que ia até a borda antiga ganha a altura do dock; as outras ficam; volta com a versão", () => {
    for (const category of LAYOUT_CATEGORIES) {
      const saved = legacy(presetLayout(category, OLD));
      expect(saved.v).toBeUndefined();
      const migrated = migrateLayout(saved, NEW);
      expect(migrated.v).toBe(LAYOUT_VERSION);
      for (const [kind, before] of Object.entries(saved.windows)) {
        const after = migrated.windows[kind as keyof Layout["windows"]];
        const touched = before.y + before.h === OLD.h - 12;
        // Só a altura muda; a janela de baixo chega à borda nova com a mesma folga.
        expect({ kind, x: after?.x, y: after?.y, w: after?.w }).toEqual({ kind, x: before.x, y: before.y, w: before.w });
        expect({ kind, h: after?.h }).toEqual({ kind, h: touched ? before.h + LEGACY_DOCK_RESERVE : before.h });
        if (touched) expect((after?.y ?? 0) + (after?.h ?? 0)).toBe(NEW.h - 12);
      }
    }
  });

  it("encostada na borda antiga (encaixe ou maximizada) também desce; já na versão, nada muda", () => {
    const snapped = legacy({ windows: { plan: { x: 0, y: 0, w: 720, h: OLD.h, z: 1, mode: "maximized", open: true } } });
    expect(migrateLayout(snapped, NEW).windows.plan).toMatchObject({ y: 0, h: NEW.h, mode: "maximized" });
    const current: Layout = { v: LAYOUT_VERSION, windows: { plan: { x: 0, y: 0, w: 720, h: OLD.h, z: 1, mode: "normal", open: true } } };
    expect(migrateLayout(current, NEW)).toBe(current);
    // Sem área medida, não migra (e não marca a versão).
    expect(migrateLayout(snapped, { w: 0, h: 0 })).toBe(snapped);
  });

  it("quando a heurística erra (o navegador mudou de tamanho), no máximo sobra um vão, nunca uma janela fora da tela", () => {
    // Gravado numa área de 900 px (com o dock) e aberto agora numa de 600: a borda "antiga" calculada
    // (600 − 84 = 516) cai no meio de uma janela qualquer.
    const small: Workspace = { w: 1024, h: 600 };
    const saved = legacy({
      windows: {
        conversation: { x: 12, y: 12, w: 400, h: 876, z: 1, mode: "normal", open: true },
        plan: { x: 420, y: 100, w: 400, h: 426, z: 2, mode: "normal", open: true },
        board: { x: 420, y: 520, w: 400, h: 70, z: 3, mode: "normal", open: true },
      },
    });
    const migrated = migrateLayout(saved, small);
    for (const [kind, win] of Object.entries(migrated.windows)) {
      // Nunca encolhe e nunca passa da borda nova por causa da migração.
      const before = saved.windows[kind as keyof Layout["windows"]];
      expect({ kind, grew: win.h >= (before?.h ?? 0) }).toEqual({ kind, grew: true });
      if (win.h !== before?.h) expect(win.y + win.h).toBeLessThanOrEqual(small.h);
    }
    // A Conversa já passava da área (fica como estava; a exibição a recorta); o Plano, perto da
    // borda calculada, cresce só até a borda nova.
    expect(migrated.windows.conversation?.h).toBe(876);
    expect(migrated.windows.plan?.h).toBe(500);
    // Gravado numa área menor e aberto numa maior: a borda calculada fica longe; sobra um vão.
    const big = migrateLayout(legacy({ windows: { plan: { x: 12, y: 12, w: 400, h: 400, z: 1, mode: "normal", open: true } } }), NEW);
    expect(big.windows.plan?.h).toBe(400);
  });

  it("current e mine: os dois migram de uma vez e só eles entram na lista para regravar (idempotente)", () => {
    let layouts = emptyCategoryLayouts();
    layouts = saveCurrent(layouts, "planning", legacy(presetLayout("planning", OLD)));
    layouts = saveMine(layouts, "execution", legacy(presetLayout("execution", OLD)));
    layouts = saveMine(layouts, "done", { v: LAYOUT_VERSION, windows: presetLayout("done", NEW).windows });
    const { layouts: next, migrated } = migrateCategoryLayouts(layouts, NEW);
    expect(migrated.map((m) => `${m.category}.${m.slot}`)).toEqual(["planning.current", "execution.mine"]);
    expect(next.planning.current?.v).toBe(LAYOUT_VERSION);
    expect(next.execution.mine?.v).toBe(LAYOUT_VERSION);
    expect(next.done.mine).toBe(layouts.done.mine);
    // O que entra é o arranjo migrado (a Conversa do planejamento chega à borda nova).
    expect(layoutForCategory(next, "planning", NEW).windows.conversation).toMatchObject({ h: NEW.h - 24 });
    expect(mineLayoutFor(next, "execution", NEW).windows.conversation).toMatchObject({ h: NEW.h - 24 });
    // Rodar de novo não muda nada nem pede regravação.
    const again = migrateCategoryLayouts(next, NEW);
    expect(again.migrated).toEqual([]);
    expect(again.layouts).toBe(next);
  });

  it("desfazer depois da migração volta ao arranjo migrado, não ao antigo com o vão do dock", () => {
    const migrated = migrateLayout(legacy(presetLayout("planning", OLD)), NEW);
    let s = createDesktopState({ windows: {} }, NEW);
    s = windowsReducer(s, { type: "applyLayout", layout: migrated, undoable: false });
    expect(canUndo(s)).toBe(false);
    s = windowsReducer(s, { type: "toggleMaximize", kind: "conversation" });
    s = windowsReducer(s, { type: "undo" });
    expect(s.layout).toBe(migrated);
    expect(s.layout.windows.conversation).toMatchObject({ y: 12, h: NEW.h - 24, mode: "normal" });
  });
});
