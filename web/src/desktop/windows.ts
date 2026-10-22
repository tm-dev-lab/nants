/*
 * Modelo puro do gerenciador de janelas do desktop (spec 093, decisão 4).
 *
 * Coordenadas relativas à área de trabalho (`Workspace`): o viewport sem a barra superior e o
 * dock, com origem (0, 0) no canto superior esquerdo. Nada sai dela: abrir, mover, redimensionar,
 * encaixar, maximizar e normalizar sempre devolvem retângulos dentro da área.
 *
 * Uma janela por tipo. A geometria `x/y/w/h` é a do modo normal; maximizada, a janela ocupa a
 * área inteira (`effectiveRect`) e a geometria normal fica guardada para restaurar.
 *
 * Geometria salva e exibida (spec 094, decisão 4): `DesktopState.layout` é o arranjo SALVO, que só
 * muda por ação do usuário. Mudar o tamanho do navegador (`setWorkspace`) é temporário: o arranjo
 * exibido (`displayedLayout`) é derivado do salvo e ajustado à área atual, sem gravar nada. Uma
 * ação de geometria com a área reduzida parte do retângulo EXIBIDO da janela-alvo e grava só ela;
 * as demais continuam com a geometria salva, e o `previous` (desfazer) guarda a salva.
 */

export const WINDOW_KINDS = [
  "conversation",
  "plan",
  "board",
  "activity",
  "files",
  "app",
  "execution",
  "costs",
  "report",
  "settings",
  "project",
  "setup",
] as const;

export type WindowKind = (typeof WINDOW_KINDS)[number];

export function isWindowKind(value: unknown): value is WindowKind {
  return typeof value === "string" && (WINDOW_KINDS as readonly string[]).includes(value);
}

/** `normal`: geometria própria; `maximized`: área inteira; `minimized`: só no dock. */
export type WindowMode = "normal" | "maximized" | "minimized";

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface WindowState extends Rect {
  /** Ordem de empilhamento: maior fica na frente. O DOM nunca é reordenado. */
  z: number;
  mode: WindowMode;
  /** Fechada = fora do desktop. A reabertura usa a cascata; a geometria guardada não é reaproveitada. */
  open: boolean;
  /** Modo a que a janela volta ao restaurar de minimizada. */
  restoreMode?: "normal" | "maximized";
}

/** Viewport sem a barra superior e o dock. */
export interface Workspace {
  w: number;
  h: number;
}

/** Arranjo das janelas de um espaço. Tipos ausentes nunca foram abertos. */
export interface Layout {
  windows: Partial<Record<WindowKind, WindowState>>;
}

export const MIN_WINDOW_W = 320;
export const MIN_WINDOW_H = 220;
/** Passo da cascata de abertura fora do preset. */
export const CASCADE_STEP = 32;
/** Margem do primeiro canto da cascata. */
export const CASCADE_ORIGIN = 16;
/** Acima disso, os `z` são compactados (1..n) preservando a ordem. */
const MAX_Z_BEFORE_COMPACT = 10_000;

/** Tamanho padrão por tipo (reduzido até caber na área de trabalho). */
export const DEFAULT_SIZES: Readonly<Record<WindowKind, { w: number; h: number }>> = {
  conversation: { w: 480, h: 640 },
  plan: { w: 640, h: 640 },
  board: { w: 680, h: 440 },
  activity: { w: 400, h: 380 },
  files: { w: 640, h: 460 },
  app: { w: 760, h: 540 },
  execution: { w: 520, h: 560 },
  costs: { w: 440, h: 420 },
  report: { w: 680, h: 620 },
  settings: { w: 760, h: 560 },
  project: { w: 540, h: 500 },
  setup: { w: 680, h: 580 },
};

export type SnapTarget =
  | "left"
  | "right"
  | "top-left"
  | "top-right"
  | "bottom-left"
  | "bottom-right"
  | "center"
  | "top";

export type SizePreset = "compact" | "medium" | "wide";

const SIZE_PRESETS: Readonly<Record<SizePreset, { w: number; h: number }>> = {
  compact: { w: 480, h: 360 },
  medium: { w: 760, h: 540 },
  wide: { w: 1100, h: 720 },
};

export interface DesktopState {
  layout: Layout;
  workspace: Workspace;
  /** Snapshot anterior à última ação de layout (para desfazer). */
  previous: Layout | null;
}

export type WindowAction =
  | { type: "open"; kind: WindowKind }
  | { type: "close"; kind: WindowKind }
  | { type: "focus"; kind: WindowKind }
  | { type: "minimize"; kind: WindowKind }
  | { type: "restore"; kind: WindowKind }
  | { type: "toggleMaximize"; kind: WindowKind }
  | { type: "move"; kind: WindowKind; x: number; y: number }
  | { type: "resize"; kind: WindowKind; w: number; h: number }
  /** Move e redimensiona de uma vez (fim do gesto ou do modo teclado). */
  | { type: "setRect"; kind: WindowKind; rect: Rect }
  | { type: "snap"; kind: WindowKind; target: SnapTarget }
  | { type: "size"; kind: WindowKind; size: SizePreset }
  | { type: "setWorkspace"; workspace: Workspace }
  /** Troca o arranjo inteiro (preset, meu layout, troca de fase, outra aba). */
  | { type: "applyLayout"; layout: Layout; undoable?: boolean }
  | { type: "undo" };

const clamp = (value: number, min: number, max: number): number => Math.min(Math.max(value, min), max);

/** Workspace válido (dimensões finitas e não negativas). */
function sanitizeWorkspace(ws: Workspace): Workspace {
  const safe = (n: number): number => (Number.isFinite(n) && n > 0 ? Math.floor(n) : 0);
  return { w: safe(ws.w), h: safe(ws.h) };
}

/** Encaixa o retângulo na área: mínimos (reduzidos se a área for menor), sem sair dela. */
export function clampRect(rect: Rect, ws: Workspace): Rect {
  const minW = Math.min(MIN_WINDOW_W, ws.w);
  const minH = Math.min(MIN_WINDOW_H, ws.h);
  const finite = (n: number, fallback: number): number => (Number.isFinite(n) ? Math.round(n) : fallback);
  const w = clamp(finite(rect.w, minW), minW, ws.w);
  const h = clamp(finite(rect.h, minH), minH, ws.h);
  const x = clamp(finite(rect.x, 0), 0, ws.w - w);
  const y = clamp(finite(rect.y, 0), 0, ws.h - h);
  return { x, y, w, h };
}

/** Retângulo exibido: a área inteira quando maximizada. */
export function effectiveRect(win: WindowState, ws: Workspace): Rect {
  if (win.mode === "maximized") return { x: 0, y: 0, w: ws.w, h: ws.h };
  return { x: win.x, y: win.y, w: win.w, h: win.h };
}

/** Janela aberta e não minimizada. */
export function isShown(win: WindowState | undefined): win is WindowState {
  return win !== undefined && win.open && win.mode !== "minimized";
}

function entries(layout: Layout): [WindowKind, WindowState][] {
  const out: [WindowKind, WindowState][] = [];
  for (const kind of WINDOW_KINDS) {
    const win = layout.windows[kind];
    if (win !== undefined) out.push([kind, win]);
  }
  return out;
}

function maxZ(layout: Layout): number {
  return entries(layout).reduce((max, [, win]) => Math.max(max, win.z), 0);
}

/** Janela visível mais à frente (a "em foco" do gerenciador). */
export function topWindow(layout: Layout): WindowKind | null {
  let best: WindowKind | null = null;
  let bestZ = -Infinity;
  for (const [kind, win] of entries(layout)) {
    if (isShown(win) && win.z > bestZ) {
      best = kind;
      bestZ = win.z;
    }
  }
  return best;
}

/** Janelas visíveis, da mais ao fundo para a mais à frente. */
export function shownByZ(layout: Layout): WindowKind[] {
  return entries(layout)
    .filter(([, win]) => isShown(win))
    .sort((a, b) => a[1].z - b[1].z)
    .map(([kind]) => kind);
}

/** Coberta por uma maximizada que está à frente dela. */
export function isCoveredByMaximized(layout: Layout, kind: WindowKind): boolean {
  const self = layout.windows[kind];
  if (!isShown(self)) return false;
  return entries(layout).some(
    ([other, win]) => other !== kind && isShown(win) && win.mode === "maximized" && win.z > self.z,
  );
}

/** Compacta os `z` (1..n) preservando a ordem, para não crescerem sem limite. */
function compactZ(layout: Layout): Layout {
  const sorted = entries(layout).sort((a, b) => a[1].z - b[1].z);
  const windows: Layout["windows"] = {};
  sorted.forEach(([kind, win], index) => {
    windows[kind] = { ...win, z: index + 1 };
  });
  return { windows };
}

function withWindow(layout: Layout, kind: WindowKind, win: WindowState): Layout {
  return { windows: { ...layout.windows, [kind]: win } };
}

function bringToFront(layout: Layout, kind: WindowKind, patch: Partial<WindowState> = {}): Layout {
  const current = layout.windows[kind];
  if (current === undefined) return layout;
  const isTop = topWindow(layout) === kind && isShown(current);
  if (isTop && Object.keys(patch).length === 0) return layout;
  const z = isTop ? current.z : maxZ(layout) + 1;
  const next = withWindow(layout, kind, { ...current, ...patch, z });
  return z > MAX_Z_BEFORE_COMPACT ? compactZ(next) : next;
}

/** Posição da cascata: primeiro canto livre (sem janela visível com o mesmo canto) que cabe. */
function cascadeRect(layout: Layout, kind: WindowKind, ws: Workspace): Rect {
  const base = DEFAULT_SIZES[kind];
  const size = clampRect({ x: 0, y: 0, w: base.w, h: base.h }, ws);
  const taken = new Set(
    entries(layout)
      .filter(([other, win]) => other !== kind && isShown(win) && win.mode === "normal")
      .map(([, win]) => `${win.x},${win.y}`),
  );
  for (let step = 0; ; step += 1) {
    const x = CASCADE_ORIGIN + step * CASCADE_STEP;
    const y = CASCADE_ORIGIN + step * CASCADE_STEP;
    if (x + size.w > ws.w || y + size.h > ws.h) break;
    if (!taken.has(`${x},${y}`)) return { x, y, w: size.w, h: size.h };
  }
  return clampRect({ x: CASCADE_ORIGIN, y: CASCADE_ORIGIN, w: size.w, h: size.h }, ws);
}

/** Geometria de um encaixe (top = área inteira; o modo maximizado é decidido no reducer). */
export function snapRect(target: SnapTarget, current: Rect, ws: Workspace): Rect {
  const halfW = Math.floor(ws.w / 2);
  const halfH = Math.floor(ws.h / 2);
  switch (target) {
    case "left":
      return clampRect({ x: 0, y: 0, w: halfW, h: ws.h }, ws);
    case "right":
      return clampRect({ x: halfW, y: 0, w: ws.w - halfW, h: ws.h }, ws);
    case "top-left":
      return clampRect({ x: 0, y: 0, w: halfW, h: halfH }, ws);
    case "top-right":
      return clampRect({ x: halfW, y: 0, w: ws.w - halfW, h: halfH }, ws);
    case "bottom-left":
      return clampRect({ x: 0, y: halfH, w: halfW, h: ws.h - halfH }, ws);
    case "bottom-right":
      return clampRect({ x: halfW, y: halfH, w: ws.w - halfW, h: ws.h - halfH }, ws);
    case "center": {
      const size = clampRect(current, ws);
      return clampRect(
        { x: Math.round((ws.w - size.w) / 2), y: Math.round((ws.h - size.h) / 2), w: size.w, h: size.h },
        ws,
      );
    }
    case "top":
      return { x: 0, y: 0, w: ws.w, h: ws.h };
  }
}

/** Tamanho pré-definido, mantendo o centro da janela. */
function sizeRect(size: SizePreset, current: Rect, ws: Workspace): Rect {
  const target = SIZE_PRESETS[size];
  const cx = current.x + current.w / 2;
  const cy = current.y + current.h / 2;
  const fitted = clampRect({ x: 0, y: 0, w: target.w, h: target.h }, ws);
  return clampRect({ x: Math.round(cx - fitted.w / 2), y: Math.round(cy - fitted.h / 2), w: fitted.w, h: fitted.h }, ws);
}

const sameRect = (a: Rect, b: Rect): boolean => a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;

/**
 * Normaliza o arranjo para a área: nenhuma janela fica fora dela nem abaixo do mínimo. Preserva a
 * identidade do que não muda (a janela que já cabe e o arranjo inteiro, se nada muda).
 */
export function normalizeLayout(layout: Layout, ws: Workspace): Layout {
  const windows: Layout["windows"] = {};
  let changed = false;
  for (const [kind, win] of entries(layout)) {
    const rect = clampRect(win, ws);
    if (sameRect(rect, win)) {
      windows[kind] = win;
    } else {
      windows[kind] = { ...win, ...rect };
      changed = true;
    }
  }
  return changed ? { windows } : layout;
}

/** Arranjo exibido: o salvo ajustado à área atual (derivado; nunca gravado). */
export function displayedLayout(state: Pick<DesktopState, "layout" | "workspace">): Layout {
  return normalizeLayout(state.layout, state.workspace);
}

export function createDesktopState(layout: Layout, workspace: Workspace): DesktopState {
  return { layout, workspace: sanitizeWorkspace(workspace), previous: null };
}

/** Ação que muda o arranjo e por isso guarda o snapshot para desfazer. */
function layoutChange(state: DesktopState, layout: Layout): DesktopState {
  if (layout === state.layout) return state;
  return { ...state, layout, previous: state.layout };
}

function updateWindow(
  state: DesktopState,
  kind: WindowKind,
  update: (win: WindowState) => WindowState | null,
): DesktopState {
  const win = state.layout.windows[kind];
  if (win === undefined) return state;
  const next = update(win);
  if (next === null) return state;
  return layoutChange(state, withWindow(state.layout, kind, next));
}

/** Janela em modo normal e visível (as únicas que se movem e redimensionam livremente). */
const isFree = (win: WindowState): boolean => win.open && win.mode === "normal";

/** Retângulo exibido da janela no modo normal (a geometria salva ajustada à área atual). */
const shownRect = (win: WindowState, ws: Workspace): Rect => clampRect(win, ws);

export function windowsReducer(state: DesktopState, action: WindowAction): DesktopState {
  const ws = state.workspace;
  switch (action.type) {
    case "open": {
      const win = state.layout.windows[action.kind];
      if (win !== undefined && win.open) {
        // Já existe: só foca (restaurando, se minimizada).
        if (win.mode === "minimized") return windowsReducer(state, { type: "restore", kind: action.kind });
        return windowsReducer(state, { type: "focus", kind: action.kind });
      }
      // Os cantos ocupados são os exibidos (o usuário vê a cascata na área atual).
      const rect = cascadeRect(normalizeLayout(state.layout, ws), action.kind, ws);
      const opened: WindowState = { ...rect, z: maxZ(state.layout) + 1, mode: "normal", open: true };
      const next = withWindow(state.layout, action.kind, opened);
      return layoutChange(state, opened.z > MAX_Z_BEFORE_COMPACT ? compactZ(next) : next);
    }
    case "close":
      return updateWindow(state, action.kind, (win) =>
        win.open ? { ...win, open: false, mode: "normal", restoreMode: undefined } : null,
      );
    case "focus": {
      const win = state.layout.windows[action.kind];
      if (win === undefined || !win.open) return state;
      if (win.mode === "minimized") return windowsReducer(state, { type: "restore", kind: action.kind });
      // Foco só reordena: não é ação de layout para desfazer.
      const layout = bringToFront(state.layout, action.kind);
      return layout === state.layout ? state : { ...state, layout };
    }
    case "minimize":
      return updateWindow(state, action.kind, (win) =>
        win.open && win.mode !== "minimized"
          ? { ...win, mode: "minimized", restoreMode: win.mode === "maximized" ? "maximized" : "normal" }
          : null,
      );
    case "restore": {
      const win = state.layout.windows[action.kind];
      if (win === undefined || !win.open || win.mode !== "minimized") return state;
      const layout = bringToFront(state.layout, action.kind, {
        mode: win.restoreMode ?? "normal",
        restoreMode: undefined,
      });
      return layoutChange(state, layout);
    }
    case "toggleMaximize": {
      const win = state.layout.windows[action.kind];
      if (win === undefined || !win.open || win.mode === "minimized") return state;
      const mode: WindowMode = win.mode === "maximized" ? "normal" : "maximized";
      return layoutChange(state, bringToFront(state.layout, action.kind, { mode }));
    }
    case "move":
      return updateWindow(state, action.kind, (win) => {
        if (!isFree(win)) return null;
        const shown = shownRect(win, ws);
        const rect = clampRect({ x: action.x, y: action.y, w: shown.w, h: shown.h }, ws);
        return rect.x === shown.x && rect.y === shown.y ? null : { ...win, ...rect };
      });
    case "resize":
      return updateWindow(state, action.kind, (win) => {
        if (!isFree(win)) return null;
        // O canto superior esquerdo fica; o tamanho é limitado pelo que sobra da área.
        const minW = Math.min(MIN_WINDOW_W, ws.w);
        const minH = Math.min(MIN_WINDOW_H, ws.h);
        const shown = shownRect(win, ws);
        const w = clamp(Math.round(action.w), minW, Math.max(minW, ws.w - shown.x));
        const h = clamp(Math.round(action.h), minH, Math.max(minH, ws.h - shown.y));
        const rect = clampRect({ x: shown.x, y: shown.y, w, h }, ws);
        return sameRect(rect, shown) ? null : { ...win, ...rect };
      });
    case "setRect":
      return updateWindow(state, action.kind, (win) => {
        if (!win.open || win.mode === "minimized") return null;
        const rect = clampRect(action.rect, ws);
        // Solta no mesmo lugar exibido: nada muda (a geometria salva não é trocada pela exibida).
        if (win.mode === "normal" && sameRect(rect, shownRect(win, ws))) return null;
        return { ...win, ...rect, mode: "normal" };
      });
    case "snap": {
      const win = state.layout.windows[action.kind];
      if (win === undefined || !win.open) return state;
      if (action.target === "top") {
        return layoutChange(state, bringToFront(state.layout, action.kind, { mode: "maximized", restoreMode: undefined }));
      }
      const rect = snapRect(action.target, shownRect(win, ws), ws);
      return layoutChange(
        state,
        bringToFront(state.layout, action.kind, { ...rect, mode: "normal", restoreMode: undefined }),
      );
    }
    case "size": {
      const win = state.layout.windows[action.kind];
      if (win === undefined || !win.open) return state;
      const rect = sizeRect(action.size, shownRect(win, ws), ws);
      return layoutChange(
        state,
        bringToFront(state.layout, action.kind, { ...rect, mode: "normal", restoreMode: undefined }),
      );
    }
    case "setWorkspace": {
      const workspace = sanitizeWorkspace(action.workspace);
      if (workspace.w === ws.w && workspace.h === ws.h) return state;
      // Temporário: só a área muda; o arranjo salvo e o do desfazer ficam como estão.
      return { ...state, workspace };
    }
    case "applyLayout": {
      // O arranjo aplicado é guardado como veio (a exibição o ajusta à área atual).
      const layout = action.layout;
      return action.undoable === false
        ? { ...state, layout, previous: null }
        : { ...state, layout, previous: state.layout };
    }
    case "undo":
      if (state.previous === null) return state;
      return { ...state, layout: state.previous, previous: null };
  }
}

export function canUndo(state: DesktopState): boolean {
  return state.previous !== null;
}

/** Valida um arranjo vindo de fora (servidor, cache, outra aba); inválido vira `null`. */
export function parseLayout(value: unknown): Layout | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const raw = (value as { windows?: unknown }).windows;
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return null;
  const windows: Layout["windows"] = {};
  for (const kind of WINDOW_KINDS) {
    if (!Object.hasOwn(raw, kind)) continue;
    const win = parseWindowState((raw as Record<string, unknown>)[kind]);
    if (win === null) return null;
    windows[kind] = win;
  }
  return { windows };
}

function parseWindowState(value: unknown): WindowState | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  const num = (n: unknown): n is number => typeof n === "number" && Number.isFinite(n);
  if (!num(v.x) || !num(v.y) || !num(v.w) || !num(v.h) || !num(v.z)) return null;
  if (v.mode !== "normal" && v.mode !== "maximized" && v.mode !== "minimized") return null;
  if (typeof v.open !== "boolean") return null;
  const win: WindowState = { x: v.x, y: v.y, w: v.w, h: v.h, z: v.z, mode: v.mode, open: v.open };
  if (v.restoreMode === "normal" || v.restoreMode === "maximized") win.restoreMode = v.restoreMode;
  return win;
}

/** Arranjo serializável (sem campos `undefined`), igual ao que vai para o servidor. */
export function serializeLayout(layout: Layout): Layout {
  const windows: Layout["windows"] = {};
  for (const [kind, win] of entries(layout)) {
    const { restoreMode, ...rest } = win;
    windows[kind] = restoreMode === undefined ? rest : { ...rest, restoreMode };
  }
  return { windows };
}
