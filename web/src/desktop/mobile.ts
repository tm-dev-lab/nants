/*
 * Modo celular (spec 093, decisão 17): modelo puro do alternador.
 * - uma janela por vez, em tela cheia; as demais continuam montadas, mas ocultas e inertes (o
 *   iframe do App e o rascunho da Conversa não se perdem ao trocar de aba);
 * - abas por categoria da fase, sempre com o Alternador (lista vertical de todas as janelas);
 * - o estado do celular fica só na seção `mobile` do ui-state do projeto (decisão 6): a vista
 *   atual, a categoria em que ela foi escolhida e a seleção própria do celular. O layout e a
 *   seleção do desktop nunca são escritos por aqui.
 */
import { localized } from "../i18n";
import type { LayoutCategory } from "./presets";
import { LAYOUT_CATEGORIES } from "./presets";
import { isWindowKind, WINDOW_KINDS, type Layout, type WindowKind, type WindowState, type Workspace } from "./windows";

/** O que ocupa a tela do celular: uma janela, o Alternador ou o início (boas-vindas, sem espaço). */
export type MobileView = WindowKind | "switcher" | "home";

export const SWITCHER: MobileView = "switcher";
export const HOME: MobileView = "home";

/** Rótulos das vistas que não são janelas (no idioma ativo). */
export const MOBILE_VIEW_LABELS: Readonly<Record<"switcher" | "home", string>> = localized({
  switcher: "mobile.switcher",
  home: "mobile.home",
});

/** Abas por categoria da fase (decisão 17). */
export const MOBILE_TABS: Readonly<Record<LayoutCategory, readonly MobileView[]>> = {
  planning: ["conversation", "plan", "switcher"],
  execution: ["conversation", "board", "switcher", "activity"],
  // 095/T03 (C-12): a Execução vira aba só no Concluído; nas outras fases, fica no Alternador.
  done: ["report", "app", "execution", "switcher", "conversation"],
};

/** Abas sem espaço (boas-vindas): o início e o Alternador (Configurações, Primeiros passos). */
export const HOME_TABS: readonly MobileView[] = ["home", "switcher"];

function isMobileView(value: unknown): value is MobileView {
  return value === "switcher" || value === "home" || isWindowKind(value);
}

/** Conteúdo da seção `mobile` do ui-state do projeto. */
export interface MobileState {
  /** Vista escolhida (`null` = a primeira aba da categoria). */
  view: MobileView | null;
  /** Categoria em que a vista foi escolhida: mudou a categoria, volta à primeira aba dela. */
  category: LayoutCategory | null;
  /** Seleção própria do celular (sobrepõe a do desktop só na leitura; valida-se ao ler). */
  selection: Readonly<Record<string, unknown>>;
}

export const EMPTY_MOBILE_STATE: MobileState = { view: null, category: null, selection: {} };

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isCategory(value: unknown): value is LayoutCategory {
  return typeof value === "string" && (LAYOUT_CATEGORIES as readonly string[]).includes(value);
}

/** Lê a seção `mobile`; valores inválidos viram o padrão (nunca lança). */
export function parseMobileState(value: unknown): MobileState {
  if (!isPlainObject(value)) return EMPTY_MOBILE_STATE;
  // "home" só existe sem espaço: num projeto, não é uma vista válida.
  const view = isMobileView(value.view) && value.view !== "home" ? value.view : null;
  const category = isCategory(value.category) ? value.category : null;
  const selection: Record<string, unknown> = {};
  if (isPlainObject(value.selection)) {
    for (const [key, item] of Object.entries(value.selection)) {
      // Nunca copiar chaves especiais do protótipo.
      if (key === "__proto__" || key === "constructor" || key === "prototype") continue;
      selection[key] = item;
    }
  }
  return { view, category, selection };
}

/** JSON gravado na seção `mobile`. */
export function serializeMobileState(state: MobileState): { view: MobileView | null; category: LayoutCategory | null; selection: Record<string, unknown> } {
  return { view: state.view, category: state.category, selection: { ...state.selection } };
}

/** Vista atual: a escolhida na mesma categoria; senão a primeira aba da categoria. */
export function mobileViewFor(state: MobileState, category: LayoutCategory): MobileView {
  const first = MOBILE_TABS[category][0] ?? "switcher";
  if (state.view === null || state.category !== category) return first;
  return state.view;
}

/** Nova seção com a vista escolhida (preserva a seleção do celular). */
export function withView(state: MobileState, view: MobileView, category: LayoutCategory): MobileState {
  return { ...state, view: view === "home" ? null : view, category };
}

/** Nova seção com a seleção do celular atualizada. */
export function withSelection(state: MobileState, patch: Readonly<Record<string, unknown>>): MobileState {
  return { ...state, selection: { ...state.selection, ...patch } };
}

/** Seleção lida no celular: a do desktop com a do celular por cima (a do desktop não muda). */
export function mergedSelection(desktop: unknown, mobile: Readonly<Record<string, unknown>>): Record<string, unknown> {
  return { ...(isPlainObject(desktop) ? desktop : {}), ...mobile };
}

/** Janela mostrada pela vista (`null` no Alternador e no início). */
export function viewWindow(view: MobileView | null): WindowKind | null {
  return view !== null && isWindowKind(view) ? view : null;
}

/**
 * Arranjo derivado para o celular, a partir do arranjo do desktop (que não muda): a janela da
 * vista fica maximizada (ocupa a área inteira) e à frente; as demais montadas (abertas no desktop
 * ou já visitadas no celular) ficam minimizadas, ou seja, ocultas e inertes. Assim a troca de
 * modo não remonta as janelas e os polls das ocultas param (`useWindowVisibility`).
 */
export function mobileLayout(desktop: Layout, active: WindowKind | null, mounted: readonly WindowKind[], ws: Workspace): Layout {
  const windows: Partial<Record<WindowKind, WindowState>> = {};
  for (const kind of WINDOW_KINDS) {
    const base = desktop.windows[kind];
    const include = kind === active || mounted.includes(kind) || base?.open === true;
    if (!include) continue;
    windows[kind] = {
      x: 0,
      y: 0,
      w: Math.max(1, ws.w),
      h: Math.max(1, ws.h),
      z: kind === active ? 2 : 1,
      open: true,
      mode: kind === active ? "maximized" : "minimized",
    };
  }
  return { windows };
}
