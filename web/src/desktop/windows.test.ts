import { describe, expect, it } from "vitest";
import { TOPBAR_HEIGHT, workspaceForViewport } from "./metrics";
import {
  CASCADE_ORIGIN,
  CASCADE_STEP,
  canUndo,
  clampRect,
  createDesktopState,
  DEFAULT_SIZES,
  displayedLayout,
  effectiveRect,
  isCoveredByMaximized,
  LAYOUT_VERSION,
  MIN_WINDOW_H,
  MIN_WINDOW_W,
  parseLayout,
  serializeLayout,
  shownByZ,
  topWindow,
  windowsReducer,
  type DesktopState,
  type Layout,
  type SnapTarget,
  type WindowAction,
  type WindowState,
  type Workspace,
} from "./windows";

const WS: Workspace = { w: 1440, h: 800 };

const win = (patch: Partial<WindowState> = {}): WindowState => ({
  x: 100,
  y: 100,
  w: 500,
  h: 400,
  z: 1,
  mode: "normal",
  open: true,
  ...patch,
});

function run(state: DesktopState, ...actions: WindowAction[]): DesktopState {
  return actions.reduce(windowsReducer, state);
}

const empty = (): DesktopState => createDesktopState({ windows: {} }, WS);

/** Nenhuma janela sai da área nem fica abaixo do mínimo. */
function expectInside(layout: Layout, ws: Workspace): void {
  for (const w of Object.values(layout.windows)) {
    expect(w.x).toBeGreaterThanOrEqual(0);
    expect(w.y).toBeGreaterThanOrEqual(0);
    expect(w.x + w.w).toBeLessThanOrEqual(ws.w);
    expect(w.y + w.h).toBeLessThanOrEqual(ws.h);
    expect(w.w).toBeGreaterThanOrEqual(Math.min(MIN_WINDOW_W, ws.w));
    expect(w.h).toBeGreaterThanOrEqual(Math.min(MIN_WINDOW_H, ws.h));
  }
}

describe("open", () => {
  it("abre com o tamanho padrão do tipo no primeiro canto da cascata e na frente", () => {
    const s = run(empty(), { type: "open", kind: "conversation" });
    const c = s.layout.windows.conversation;
    expect(c).toMatchObject({ x: CASCADE_ORIGIN, y: CASCADE_ORIGIN, ...DEFAULT_SIZES.conversation, mode: "normal", open: true });
    expect(topWindow(s.layout)).toBe("conversation");
  });

  it("cascata de 32 px a partir do primeiro canto livre", () => {
    const s = run(empty(), { type: "open", kind: "conversation" }, { type: "open", kind: "plan" }, { type: "open", kind: "costs" });
    expect(s.layout.windows.plan).toMatchObject({ x: CASCADE_ORIGIN + CASCADE_STEP, y: CASCADE_ORIGIN + CASCADE_STEP });
    expect(s.layout.windows.costs).toMatchObject({ x: CASCADE_ORIGIN + 2 * CASCADE_STEP, y: CASCADE_ORIGIN + 2 * CASCADE_STEP });
    // Fechar a primeira libera o primeiro canto.
    const t = run(s, { type: "close", kind: "conversation" }, { type: "open", kind: "board" });
    expect(t.layout.windows.board).toMatchObject({ x: CASCADE_ORIGIN, y: CASCADE_ORIGIN });
    expect(shownByZ(t.layout)).toEqual(["plan", "costs", "board"]);
  });

  it("reduz o tamanho padrão até caber na área", () => {
    const small = createDesktopState({ windows: {} }, { w: 600, h: 400 });
    const s = run(small, { type: "open", kind: "app" });
    expectInside(s.layout, { w: 600, h: 400 });
    expect(s.layout.windows.app?.w).toBeLessThanOrEqual(600);
  });

  it("se já existe, só foca (e restaura se minimizada)", () => {
    const s = run(empty(), { type: "open", kind: "conversation" }, { type: "open", kind: "plan" });
    const before = s.layout.windows.conversation;
    const focused = run(s, { type: "open", kind: "conversation" });
    expect(topWindow(focused.layout)).toBe("conversation");
    expect(focused.layout.windows.conversation).toMatchObject({ x: before?.x, y: before?.y, w: before?.w, h: before?.h });
    const restored = run(s, { type: "minimize", kind: "plan" }, { type: "open", kind: "plan" });
    expect(restored.layout.windows.plan?.mode).toBe("normal");
    expect(topWindow(restored.layout)).toBe("plan");
  });
});

describe("close, focus, minimize, restore, maximize", () => {
  const base = (): DesktopState =>
    createDesktopState({ windows: { conversation: win({ z: 1 }), plan: win({ x: 300, z: 2 }) } }, WS);

  it("close tira do desktop; reabrir usa a cascata", () => {
    const s = run(base(), { type: "close", kind: "plan" });
    expect(s.layout.windows.plan).toMatchObject({ open: false, x: 300 });
    expect(topWindow(s.layout)).toBe("conversation");
    const reopened = run(s, { type: "open", kind: "plan" });
    expect(reopened.layout.windows.plan).toMatchObject({ open: true, x: CASCADE_ORIGIN, y: CASCADE_ORIGIN });
  });

  it("focus traz para a frente sem mexer na geometria nem gravar desfazer", () => {
    const s = run(base(), { type: "focus", kind: "conversation" });
    expect(topWindow(s.layout)).toBe("conversation");
    expect(s.previous).toBeNull();
    // Focar a que já está na frente não muda nada.
    expect(run(s, { type: "focus", kind: "conversation" })).toBe(s);
  });

  it("minimize e restore (volta ao modo anterior, na frente)", () => {
    const s = run(base(), { type: "toggleMaximize", kind: "conversation" }, { type: "minimize", kind: "conversation" });
    expect(s.layout.windows.conversation?.mode).toBe("minimized");
    expect(topWindow(s.layout)).toBe("plan");
    const r = run(s, { type: "restore", kind: "conversation" });
    expect(r.layout.windows.conversation?.mode).toBe("maximized");
    expect(topWindow(r.layout)).toBe("conversation");
    // Focar uma minimizada também restaura.
    const f = run(s, { type: "focus", kind: "conversation" });
    expect(f.layout.windows.conversation?.mode).toBe("maximized");
  });

  it("toggleMaximize ocupa a área inteira e preserva a geometria normal", () => {
    const s = run(base(), { type: "toggleMaximize", kind: "conversation" });
    const c = s.layout.windows.conversation;
    expect(c?.mode).toBe("maximized");
    expect(c && effectiveRect(c, WS)).toEqual({ x: 0, y: 0, w: WS.w, h: WS.h });
    expect(isCoveredByMaximized(s.layout, "plan")).toBe(true);
    const back = run(s, { type: "toggleMaximize", kind: "conversation" });
    expect(back.layout.windows.conversation).toMatchObject({ mode: "normal", x: 100, y: 100, w: 500, h: 400 });
  });

  it("ações em janela inexistente ou fechada não mudam o estado", () => {
    const s = base();
    expect(run(s, { type: "minimize", kind: "files" })).toBe(s);
    expect(run(s, { type: "restore", kind: "plan" })).toBe(s);
    const closed = run(s, { type: "close", kind: "plan" });
    expect(run(closed, { type: "focus", kind: "plan" })).toBe(closed);
    expect(run(closed, { type: "close", kind: "plan" })).toBe(closed);
  });
});

describe("move e resize", () => {
  const base = (): DesktopState => createDesktopState({ windows: { conversation: win() } }, WS);

  it("move dentro da área (limitada nas bordas)", () => {
    expect(run(base(), { type: "move", kind: "conversation", x: 200, y: 50 }).layout.windows.conversation).toMatchObject({ x: 200, y: 50 });
    const out = run(base(), { type: "move", kind: "conversation", x: -90, y: 5000 }).layout.windows.conversation;
    expect(out).toMatchObject({ x: 0, y: WS.h - 400 });
  });

  it("resize respeita o mínimo de 320×220 e o que sobra da área", () => {
    const tiny = run(base(), { type: "resize", kind: "conversation", w: 10, h: 10 }).layout.windows.conversation;
    expect(tiny).toMatchObject({ w: MIN_WINDOW_W, h: MIN_WINDOW_H });
    const huge = run(base(), { type: "resize", kind: "conversation", w: 9999, h: 9999 }).layout.windows.conversation;
    expect(huge).toMatchObject({ x: 100, y: 100, w: WS.w - 100, h: WS.h - 100 });
  });

  it("não move nem redimensiona maximizada ou minimizada", () => {
    const max = run(base(), { type: "toggleMaximize", kind: "conversation" });
    expect(run(max, { type: "move", kind: "conversation", x: 0, y: 0 })).toBe(max);
    expect(run(max, { type: "resize", kind: "conversation", w: 400, h: 300 })).toBe(max);
  });

  it("setRect (fim do gesto) grava a geometria e volta ao modo normal", () => {
    const s = run(base(), { type: "setRect", kind: "conversation", rect: { x: 10, y: 20, w: 330, h: 230 } });
    expect(s.layout.windows.conversation).toMatchObject({ x: 10, y: 20, w: 330, h: 230, mode: "normal" });
  });
});

describe("snap e size", () => {
  const base = (): DesktopState => createDesktopState({ windows: { conversation: win() } }, WS);
  const snap = (target: SnapTarget) =>
    run(base(), { type: "snap", kind: "conversation", target }).layout.windows.conversation;

  it("metades, quartos, centro e topo = maximizar", () => {
    expect(snap("left")).toMatchObject({ x: 0, y: 0, w: 720, h: 800 });
    expect(snap("right")).toMatchObject({ x: 720, y: 0, w: 720, h: 800 });
    expect(snap("top-left")).toMatchObject({ x: 0, y: 0, w: 720, h: 400 });
    expect(snap("top-right")).toMatchObject({ x: 720, y: 0, w: 720, h: 400 });
    expect(snap("bottom-left")).toMatchObject({ x: 0, y: 400, w: 720, h: 400 });
    expect(snap("bottom-right")).toMatchObject({ x: 720, y: 400, w: 720, h: 400 });
    expect(snap("center")).toMatchObject({ x: 470, y: 200, w: 500, h: 400 });
    expect(snap("top")).toMatchObject({ mode: "maximized" });
  });

  it("encaixe em área pequena respeita mínimo e área", () => {
    const small = createDesktopState({ windows: { conversation: win() } }, { w: 500, h: 300 });
    const s = run(small, { type: "snap", kind: "conversation", target: "bottom-right" });
    expectInside(s.layout, { w: 500, h: 300 });
  });

  it("tamanhos compacta, média e ampla (mantendo o centro, cabendo na área)", () => {
    const c = run(base(), { type: "size", kind: "conversation", size: "compact" }).layout.windows.conversation;
    expect(c).toMatchObject({ w: 480, h: 360, x: 110, y: 120 });
    const m = run(base(), { type: "size", kind: "conversation", size: "medium" }).layout.windows.conversation;
    expect(m).toMatchObject({ w: 760, h: 540 });
    const w = run(base(), { type: "size", kind: "conversation", size: "wide" }).layout.windows.conversation;
    expect(w).toMatchObject({ w: 1100, h: 720 });
    expectInside(run(base(), { type: "size", kind: "conversation", size: "wide" }).layout, WS);
  });
});

describe("normalização ao mudar a área de trabalho", () => {
  it("nada sai da área exibida nem fica abaixo do mínimo", () => {
    const s = createDesktopState(
      { windows: { conversation: win({ x: 1200, y: 700, w: 600, h: 500 }), plan: win({ x: -50, y: -50, w: 100, h: 100 }) } },
      WS,
    );
    expectInside(displayedLayout(s), WS);
    const smaller = run(s, { type: "setWorkspace", workspace: { w: 900, h: 600 } });
    expectInside(displayedLayout(smaller), { w: 900, h: 600 });
    // A área menor que o mínimo reduz o mínimo à própria área.
    const tiny = run(s, { type: "setWorkspace", workspace: { w: 200, h: 150 } });
    expectInside(displayedLayout(tiny), { w: 200, h: 150 });
  });

  it("mesma área não muda o estado; valores inválidos viram 0", () => {
    const s = createDesktopState({ windows: { conversation: win() } }, WS);
    expect(run(s, { type: "setWorkspace", workspace: { ...WS } })).toBe(s);
    expect(clampRect({ x: Number.NaN, y: 1, w: Number.POSITIVE_INFINITY, h: 300 }, WS)).toMatchObject({ x: 0 });
  });
});

describe("desfazer", () => {
  it("desfaz a última ação de layout (uma vez)", () => {
    const s = run(createDesktopState({ windows: { conversation: win() } }, WS), { type: "move", kind: "conversation", x: 600, y: 300 });
    expect(canUndo(s)).toBe(true);
    const u = run(s, { type: "undo" });
    expect(u.layout.windows.conversation).toMatchObject({ x: 100, y: 100 });
    expect(canUndo(u)).toBe(false);
    expect(run(u, { type: "undo" })).toBe(u);
  });

  it("applyLayout é desfazível; com undoable=false limpa o desfazer", () => {
    const s = createDesktopState({ windows: { conversation: win() } }, WS);
    const applied = run(s, { type: "applyLayout", layout: { windows: { plan: win({ x: 5 }) } } });
    expect(run(applied, { type: "undo" }).layout.windows.conversation).toBeDefined();
    const silent = run(applied, { type: "applyLayout", layout: { windows: {} }, undoable: false });
    expect(canUndo(silent)).toBe(false);
  });

  it("desfazer depois de a área mudar: exibido dentro da área, salvo intacto", () => {
    const s = run(createDesktopState({ windows: { conversation: win({ x: 900, w: 500 }) } }, WS), {
      type: "move",
      kind: "conversation",
      x: 0,
      y: 0,
    });
    const smaller = run(s, { type: "setWorkspace", workspace: { w: 800, h: 600 } }, { type: "undo" });
    expectInside(displayedLayout(smaller), { w: 800, h: 600 });
    expect(smaller.layout.windows.conversation).toMatchObject({ x: 900, w: 500 });
  });
});

describe("parseLayout / serializeLayout", () => {
  it("valida o que vem de fora e rejeita estado inválido", () => {
    const layout: Layout = { windows: { conversation: win({ restoreMode: "maximized", mode: "minimized" }) } };
    // O que vai ao servidor leva a versão (137/T02), e ela volta na leitura.
    const sent = JSON.parse(JSON.stringify(serializeLayout(layout))) as unknown;
    expect(sent).toMatchObject({ v: LAYOUT_VERSION });
    expect(parseLayout(sent)).toEqual({ v: LAYOUT_VERSION, ...layout });
    expect(parseLayout(null)).toBeNull();
    expect(parseLayout({ windows: [] })).toBeNull();
    expect(parseLayout({ windows: { conversation: { ...win(), mode: "flutuante" } } })).toBeNull();
    expect(parseLayout({ windows: { conversation: { ...win(), x: "1" } } })).toBeNull();
    // Tipo desconhecido é ignorado.
    expect(parseLayout({ windows: { antiga: win() } })).toEqual({ windows: {} });
  });

  it("versão: sem ela (ou com outra), o arranjo é do tempo do dock e vem sem `v`", () => {
    expect(LAYOUT_VERSION).toBe(2);
    expect(parseLayout({ windows: { plan: win() } })).toEqual({ windows: { plan: win() } });
    expect(parseLayout({ v: 1, windows: { plan: win() } })).toEqual({ windows: { plan: win() } });
    expect(parseLayout({ v: "2", windows: { plan: win() } })).toEqual({ windows: { plan: win() } });
    expect(parseLayout({ v: 2, windows: { plan: win() } })).toEqual({ v: 2, windows: { plan: win() } });
  });

  it("maximizar ocupa a área nova inteira (sem o dock): até a borda de baixo", () => {
    const ws = workspaceForViewport(1440, 860);
    let s = createDesktopState({ windows: { plan: win() } }, ws);
    s = run(s, { type: "toggleMaximize", kind: "plan" });
    const plan = s.layout.windows.plan;
    if (plan === undefined) throw new Error("plano ausente");
    expect(effectiveRect(plan, s.workspace)).toEqual({ x: 0, y: 0, w: 1440, h: 860 - TOPBAR_HEIGHT });
    // Encaixe à esquerda e redimensionar também vão até a borda nova.
    s = run(s, { type: "toggleMaximize", kind: "plan" }, { type: "snap", kind: "plan", target: "left" });
    expect(s.layout.windows.plan).toMatchObject({ y: 0, h: 860 - TOPBAR_HEIGHT });
    s = run(s, { type: "setRect", kind: "plan", rect: { x: 0, y: 400, w: 500, h: 300 } }, { type: "resize", kind: "plan", w: 500, h: 9_999 });
    expect(s.layout.windows.plan).toMatchObject({ y: 400, h: 860 - TOPBAR_HEIGHT - 400 });
  });

  it("z é compactado quando cresce demais, sem mudar a ordem", () => {
    let s = createDesktopState({ windows: { conversation: win({ z: 9_999 }), plan: win({ z: 9_998 }) } }, WS);
    s = run(s, { type: "focus", kind: "plan" }, { type: "focus", kind: "conversation" });
    expect(shownByZ(s.layout)).toEqual(["plan", "conversation"]);
    expect(Math.max(...Object.values(s.layout.windows).map((w) => w.z))).toBeLessThan(100);
  });
});

describe("tamanho do navegador é temporário (spec 094, decisão 4)", () => {
  const BIG: Workspace = { w: 1440, h: 900 };
  const SMALL: Workspace = { w: 900, h: 700 };
  const saved = (): Layout => ({
    windows: {
      conversation: win({ x: 40, y: 40, w: 600, h: 760, z: 1 }),
      plan: win({ x: 700, y: 60, w: 700, h: 800, z: 2 }),
    },
  });

  it("1440×900 → 900×700 → 1440×900 volta à mesma geometria; o salvo nunca muda", () => {
    const start = createDesktopState(saved(), BIG);
    const before = displayedLayout(start);
    const small = run(start, { type: "setWorkspace", workspace: SMALL });
    // Exibida ajustada à área menor; salva intacta (mesma identidade) e sem desfazer novo.
    expectInside(displayedLayout(small), SMALL);
    expect(small.layout).toBe(start.layout);
    expect(small.previous).toBe(start.previous);
    const back = run(small, { type: "setWorkspace", workspace: BIG });
    expect(back.layout).toBe(start.layout);
    expect(displayedLayout(back)).toEqual(before);
  });

  it("1440 → 900, mover A, voltar a 1440: B volta ao original e A fica onde foi solta", () => {
    const start = createDesktopState(saved(), BIG);
    const small = run(start, { type: "setWorkspace", workspace: SMALL });
    const shownA = displayedLayout(small).windows.plan;
    if (shownA === undefined) throw new Error("plano ausente");
    // A parte do retângulo EXIBIDO (largura e altura ajustadas à área menor).
    const moved = run(small, { type: "move", kind: "plan", x: 100, y: 0 });
    expect(moved.layout.windows.plan).toMatchObject({ x: 100, y: 0, w: shownA.w, h: shownA.h });
    // Só A foi gravada: B continua com a geometria salva (mesmo objeto).
    expect(moved.layout.windows.conversation).toBe(start.layout.windows.conversation);
    // O desfazer guarda a geometria salva.
    expect(moved.previous).toBe(start.layout);
    const back = run(moved, { type: "setWorkspace", workspace: BIG });
    const shown = displayedLayout(back);
    expect(shown.windows.conversation).toMatchObject({ x: 40, y: 40, w: 600, h: 760 });
    expect(shown.windows.plan).toMatchObject({ x: 100, y: 0, w: shownA.w, h: shownA.h });
    // Desfazer devolve a salva de antes (não a encolhida).
    expect(displayedLayout(run(back, { type: "undo" })).windows.plan).toMatchObject({ x: 700, y: 60, w: 700, h: 800 });
  });

  it("redimensionar, encaixar e tamanho partem do exibido e gravam só a janela-alvo", () => {
    const small = run(createDesktopState(saved(), BIG), { type: "setWorkspace", workspace: SMALL });
    const resized = run(small, { type: "resize", kind: "plan", w: 400, h: 300 });
    const shownBefore = displayedLayout(small).windows.plan;
    expect(resized.layout.windows.plan).toMatchObject({ x: shownBefore?.x, y: shownBefore?.y, w: 400, h: 300 });
    expect(resized.layout.windows.conversation).toBe(small.layout.windows.conversation);
    const snapped = run(small, { type: "snap", kind: "plan", target: "left" });
    expect(snapped.layout.windows.plan).toMatchObject({ x: 0, y: 0, w: 450, h: 700, mode: "normal" });
    expect(snapped.layout.windows.conversation).toBe(small.layout.windows.conversation);
    const centered = run(small, { type: "snap", kind: "plan", target: "center" });
    expectInside({ windows: { plan: centered.layout.windows.plan ?? win() } }, SMALL);
    const sized = run(small, { type: "size", kind: "plan", size: "compact" });
    expectInside({ windows: { plan: sized.layout.windows.plan ?? win() } }, SMALL);
    expect(sized.layout.windows.conversation).toBe(small.layout.windows.conversation);
  });

  it("maximizar e restaurar com a área reduzida não trocam a geometria salva", () => {
    const small = run(createDesktopState(saved(), BIG), { type: "setWorkspace", workspace: SMALL });
    const max = run(small, { type: "toggleMaximize", kind: "plan" });
    expect(effectiveRect(max.layout.windows.plan ?? win(), SMALL)).toEqual({ x: 0, y: 0, w: 900, h: 700 });
    const restored = run(max, { type: "toggleMaximize", kind: "plan" }, { type: "setWorkspace", workspace: BIG });
    expect(displayedLayout(restored).windows.plan).toMatchObject({ mode: "normal", x: 700, y: 60, w: 700, h: 800 });
  });

  it("soltar no mesmo lugar exibido não grava a geometria ajustada", () => {
    const small = run(createDesktopState(saved(), BIG), { type: "setWorkspace", workspace: SMALL });
    const shown = displayedLayout(small).windows.plan;
    if (shown === undefined) throw new Error("plano ausente");
    const same = run(small, { type: "setRect", kind: "plan", rect: { x: shown.x, y: shown.y, w: shown.w, h: shown.h } });
    expect(same).toBe(small);
    expect(run(small, { type: "move", kind: "plan", x: shown.x, y: shown.y })).toBe(small);
  });

  it("aplicar um arranjo guarda o salvo como veio; a exibição o ajusta", () => {
    const small = createDesktopState({ windows: {} }, SMALL);
    const applied = run(small, { type: "applyLayout", layout: saved(), undoable: false });
    expect(applied.layout.windows.plan).toMatchObject({ x: 700, w: 700 });
    expectInside(displayedLayout(applied), SMALL);
  });

  it("a cascata de abertura usa os cantos exibidos", () => {
    const s = run(createDesktopState({ windows: {} }, SMALL), { type: "open", kind: "conversation" }, { type: "open", kind: "plan" });
    expectInside(displayedLayout(s), SMALL);
    expect(s.layout.windows.plan).toMatchObject({ x: CASCADE_ORIGIN + CASCADE_STEP });
  });
});
