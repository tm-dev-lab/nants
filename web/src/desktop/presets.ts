/*
 * Layouts por fase (spec 093, decisão 5): categorias, presets escalados à área de trabalho e
 * o par "arranjo atual" / "meu layout" guardado por categoria. Funções puras.
 */
import { localized } from "../i18n";
import type { ProjectPhase } from "../store";
import { LEGACY_DOCK_RESERVE } from "./metrics";
import {
  LAYOUT_VERSION,
  normalizeLayout,
  parseLayout,
  WINDOW_KINDS,
  type Layout,
  type WindowKind,
  type Workspace,
} from "./windows";

export type LayoutCategory = "planning" | "execution" | "done";

export const LAYOUT_CATEGORIES: readonly LayoutCategory[] = ["planning", "execution", "done"];

export const CATEGORY_LABELS: Readonly<Record<LayoutCategory, string>> = localized({
  planning: "layout.category.planning",
  execution: "layout.category.execution",
  done: "layout.category.done",
});

const PHASE_CATEGORY: Readonly<Record<ProjectPhase, LayoutCategory>> = {
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

/** Categoria de layout de cada uma das 9 fases. */
export function categoryOf(phase: ProjectPhase): LayoutCategory {
  return PHASE_CATEGORY[phase];
}

/** Retângulo em frações da área de trabalho (0..1). */
interface Fraction {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Espaço entre janelas e bordas nos presets, em px. */
const PRESET_GAP = 12;

/**
 * Presets em frações (a ordem define o empilhamento: a última fica na frente).
 * Planejamento: Conversa + Plano. Execução: Conversa + Quadro + Atividade + Arquivos.
 * Concluído: Relatório + App + Conversa + Execução. As frações garantem o mínimo de 320×220 numa área
 * de 1024×640 (a menor do modo desktop).
 */
const PRESETS: Readonly<Record<LayoutCategory, readonly [WindowKind, Fraction][]>> = {
  planning: [
    ["conversation", { x: 0, y: 0, w: 0.42, h: 1 }],
    ["plan", { x: 0.42, y: 0, w: 0.58, h: 1 }],
  ],
  execution: [
    ["files", { x: 0.67, y: 0.5, w: 0.33, h: 0.5 }],
    ["activity", { x: 0.34, y: 0.5, w: 0.33, h: 0.5 }],
    ["board", { x: 0.34, y: 0, w: 0.66, h: 0.5 }],
    ["conversation", { x: 0, y: 0, w: 0.34, h: 1 }],
  ],
  // 095/T03 (C-05): Relatório à esquerda, App em cima e, embaixo, Conversa e Execução lado a lado.
  done: [
    ["execution", { x: 0.675, y: 0.6, w: 0.325, h: 0.4 }],
    ["conversation", { x: 0.35, y: 0.6, w: 0.325, h: 0.4 }],
    ["app", { x: 0.35, y: 0, w: 0.65, h: 0.6 }],
    ["report", { x: 0, y: 0, w: 0.35, h: 1 }],
  ],
};

/** Janelas do preset de cada categoria (na ordem do empilhamento). */
export function presetWindows(category: LayoutCategory): WindowKind[] {
  return PRESETS[category].map(([kind]) => kind);
}

/** Preset da categoria, escalado à área de trabalho (com folga entre as janelas). */
export function presetLayout(category: LayoutCategory, ws: Workspace): Layout {
  const windows: Layout["windows"] = {};
  const half = PRESET_GAP / 2;
  const edge = (from: number, size: number, total: number): [number, number] => {
    const start = from <= 0 ? PRESET_GAP : Math.round(from * total) + half;
    const end = from + size >= 1 ? total - PRESET_GAP : Math.round((from + size) * total) - half;
    return [start, end - start];
  };
  PRESETS[category].forEach(([kind, f], index) => {
    const [x, w] = edge(f.x, f.w, ws.w);
    const [y, h] = edge(f.y, f.h, ws.h);
    windows[kind] = { x, y, w, h, z: index + 1, mode: "normal", open: true };
  });
  return normalizeLayout({ windows }, ws);
}

/** Por categoria: o arranjo atual (sempre salvo) e o meu layout (só por "Salvar"). */
interface CategoryLayout {
  current: Layout | null;
  mine: Layout | null;
}

export type CategoryLayouts = Record<LayoutCategory, CategoryLayout>;

export function emptyCategoryLayouts(): CategoryLayouts {
  return {
    planning: { current: null, mine: null },
    execution: { current: null, mine: null },
    done: { current: null, mine: null },
  };
}

/**
 * Migração única de um arranjo gravado com o dock (sem versão; spec 137/T02). A geometria é gravada
 * em px absolutos: sem migrar, toda janela que ia até o dock ficaria com o vão dele embaixo.
 * - A janela cuja borda de baixo estava a até `PRESET_GAP` da borda antiga (`ws.h -
 *   LEGACY_DOCK_RESERVE`) ganha a altura do dock; as outras ficam como estão.
 * - Nunca passa da borda nova (`ws.h`) nem encolhe: se o navegador mudou de tamanho entre a
 *   gravação e agora e a heurística erra, no máximo sobra um vão, nunca uma janela fora da tela.
 * - Devolve o arranjo com a versão (quem chama o grava assim); um arranjo já na versão atual
 *   volta como veio. Sem área medida (`ws.h` zero), não migra.
 */
export function migrateLayout(layout: Layout, ws: Workspace): Layout {
  if (layout.v === LAYOUT_VERSION || ws.h <= 0) return layout;
  const oldBottom = ws.h - LEGACY_DOCK_RESERVE;
  const windows: Layout["windows"] = {};
  for (const kind of WINDOW_KINDS) {
    const win = layout.windows[kind];
    if (win === undefined) continue;
    if (Math.abs(oldBottom - (win.y + win.h)) > PRESET_GAP) {
      windows[kind] = win;
      continue;
    }
    const h = Math.max(win.h, Math.min(win.h + LEGACY_DOCK_RESERVE, ws.h - win.y));
    windows[kind] = h === win.h ? win : { ...win, h };
  }
  return { v: LAYOUT_VERSION, windows };
}

type LayoutSlot = keyof CategoryLayout;

/** Arranjo que a migração mudou (para gravar de volta, já com a versão). */
interface MigratedLayout {
  category: LayoutCategory;
  slot: LayoutSlot;
  layout: Layout;
}

/**
 * Migra os arranjos gravados de todas as categorias (o atual e o meu) para a área sem dock.
 * Idempotente: o que já está na versão atual passa direto e fica fora de `migrated`.
 */
export function migrateCategoryLayouts(layouts: CategoryLayouts, ws: Workspace): { layouts: CategoryLayouts; migrated: MigratedLayout[] } {
  const out = emptyCategoryLayouts();
  const migrated: MigratedLayout[] = [];
  for (const category of LAYOUT_CATEGORIES) {
    for (const slot of ["current", "mine"] as const) {
      const stored = layouts[category][slot];
      const layout = stored === null ? null : migrateLayout(stored, ws);
      out[category][slot] = layout;
      if (layout !== null && layout !== stored) migrated.push({ category, slot, layout });
    }
  }
  return migrated.length === 0 ? { layouts, migrated } : { layouts: out, migrated };
}

/** Valida o que veio do servidor/cache; o que for inválido volta a `null` (o padrão). */
export function parseCategoryLayouts(value: unknown): CategoryLayouts {
  const out = emptyCategoryLayouts();
  if (typeof value !== "object" || value === null || Array.isArray(value)) return out;
  for (const category of LAYOUT_CATEGORIES) {
    if (!Object.hasOwn(value, category)) continue;
    const entry = (value as Record<string, unknown>)[category];
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) continue;
    const e = entry as Record<string, unknown>;
    out[category] = {
      current: Object.hasOwn(e, "current") ? parseLayout(e.current) : null,
      mine: Object.hasOwn(e, "mine") ? parseLayout(e.mine) : null,
    };
  }
  return out;
}

/** Aplicar o padrão da categoria. */
export function defaultLayoutFor(category: LayoutCategory, ws: Workspace): Layout {
  return presetLayout(category, ws);
}

/**
 * Aplicar o meu layout (sem meu layout salvo, o padrão). Um arranjo salvo volta como foi salvo: a
 * exibição o ajusta à área atual sem gravar (spec 094, decisão 4).
 */
export function mineLayoutFor(layouts: CategoryLayouts, category: LayoutCategory, ws: Workspace): Layout {
  const mine = layouts[category].mine;
  return mine === null ? presetLayout(category, ws) : mine;
}

/** Arranjo ao entrar na categoria: o atual salvo; senão o meu; senão o padrão. */
export function layoutForCategory(layouts: CategoryLayouts, category: LayoutCategory, ws: Workspace): Layout {
  const current = layouts[category].current;
  if (current !== null) return current;
  return mineLayoutFor(layouts, category, ws);
}

/** Grava o arranjo atual da categoria (sempre salvo). */
export function saveCurrent(layouts: CategoryLayouts, category: LayoutCategory, layout: Layout): CategoryLayouts {
  return { ...layouts, [category]: { ...layouts[category], current: layout } };
}

/** "Salvar como meu layout desta fase". */
export function saveMine(layouts: CategoryLayouts, category: LayoutCategory, layout: Layout): CategoryLayouts {
  return { ...layouts, [category]: { ...layouts[category], mine: layout } };
}

/** "Restaurar o padrão": esquece o meu layout e devolve o preset para aplicar. */
export function restoreDefault(
  layouts: CategoryLayouts,
  category: LayoutCategory,
  ws: Workspace,
): { layouts: CategoryLayouts; layout: Layout } {
  const layout = presetLayout(category, ws);
  return { layouts: { ...layouts, [category]: { current: layout, mine: null } }, layout };
}

interface PhaseLayoutState {
  layouts: CategoryLayouts;
  /** Arranjo em uso agora. */
  layout: Layout;
  workspace: Workspace;
}

interface PhaseChange {
  category: LayoutCategory;
  layouts: CategoryLayouts;
  layout: Layout;
}

/**
 * Troca de fase: só muda o arranjo quando a CATEGORIA muda. Guarda o arranjo atual na
 * categoria anterior e devolve o da nova. Mesma categoria → `null` (nada a fazer).
 * `prev = null` é a primeira carga: devolve o arranjo da categoria de `next`.
 */
export function onPhaseChange(
  prev: ProjectPhase | null,
  next: ProjectPhase,
  state: PhaseLayoutState,
): PhaseChange | null {
  const nextCategory = categoryOf(next);
  if (prev !== null && categoryOf(prev) === nextCategory) return null;
  const layouts = prev === null ? state.layouts : saveCurrent(state.layouts, categoryOf(prev), state.layout);
  return { category: nextCategory, layouts, layout: layoutForCategory(layouts, nextCategory, state.workspace) };
}
