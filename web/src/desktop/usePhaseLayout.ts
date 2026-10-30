/*
 * Layout por fase de um espaço (spec 093, decisões 2 e 5), ligando o modelo puro da T03
 * (`presets.ts`) ao ui-state do projeto:
 * - ao abrir o espaço (ui-state lido e estado do projeto carregado), aplica o arranjo da
 *   categoria da fase: o atual salvo; senão o meu layout; senão o padrão;
 * - o arranjo só troca quando a CATEGORIA da fase muda;
 * - o arranjo atual é sempre salvo (`layouts.<categoria>.current`, debounce ≤ 1/s no cliente);
 * - comandos do menu Layout (aplicar o meu, aplicar o padrão, salvar, restaurar o padrão);
 * - modo celular (T10): nada do layout é gravado (o celular só grava a seção `mobile`) e a área
 *   de trabalho do celular não é usada; ao voltar ao desktop, o arranjo salvo da categoria é
 *   reaplicado na área real, preservando o layout do desktop;
 * - arranjos gravados com o dock (sem versão, spec 137/T02) passam pela migração única na área real
 *   (`migrateCategoryLayouts`) e, na primeira aplicação no desktop, são gravados de volta com a versão.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ProjectPhase } from "../store";
import {
  categoryOf,
  defaultLayoutFor,
  layoutForCategory,
  migrateCategoryLayouts,
  mineLayoutFor,
  onPhaseChange,
  parseCategoryLayouts,
  restoreDefault,
  type CategoryLayouts,
  type LayoutCategory,
} from "./presets";
import type { LayoutCommand } from "./topbar/LayoutMenu";
import { t } from "../i18n";
import { getSection, UI_SECTIONS } from "./uiState";
import type { UiStateHandle } from "./useUiState";
import { serializeLayout, type DesktopState, type Layout, type WindowAction, type Workspace } from "./windows";

const LAYOUTS_SECTION = "layouts";

interface PhaseLayout {
  layouts: CategoryLayouts;
  category: LayoutCategory;
  /** O arranjo da fase já foi aplicado (o espaço está pronto). */
  initialized: boolean;
  /** Executa um comando do menu Layout; devolve o texto do aviso e se ele pode ser desfeito. */
  command: (command: LayoutCommand) => { message: string; undoable: boolean };
  /** "Restaurar o layout da fase" (cartão sem janelas): o meu layout, senão o padrão. */
  restorePhaseLayout: () => void;
}

interface PhaseLayoutOptions {
  ui: UiStateHandle | null;
  state: DesktopState;
  dispatch: (action: WindowAction) => void;
  phase: ProjectPhase;
  /** Estado do projeto carregado (a fase é confiável). */
  ready: boolean;
  /** Mede a área de trabalho real (a primeira aplicação acontece antes do re-render medido). */
  measure?: () => Workspace | null;
  /** Modo celular: não grava o layout nem mede a área (a do celular não é a do desktop). */
  mobile?: boolean;
}

export function usePhaseLayout({ ui, state, dispatch, phase, ready, measure, mobile = false }: PhaseLayoutOptions): PhaseLayout {
  const raw = ui === null ? undefined : getSection(ui.snapshot.data, LAYOUTS_SECTION);
  const layouts = useMemo(() => parseCategoryLayouts(raw), [raw]);
  const uiReady = ui?.snapshot.ready === true;
  const [initialized, setInitialized] = useState(false);
  const prevPhase = useRef<ProjectPhase | null>(null);
  const categoryRef = useRef<LayoutCategory>(categoryOf(phase));
  const stateRef = useRef(state);
  stateRef.current = state;
  const layoutsRef = useRef(layouts);
  layoutsRef.current = layouts;
  const mobileRef = useRef(mobile);
  mobileRef.current = mobile;
  const measureDesktop = useCallback((): Workspace | null => (mobileRef.current ? null : (measure?.() ?? null)), [measure]);
  /** Os arranjos gravados, migrados para a área sem dock (os da versão atual passam direto). */
  const storedLayouts = useCallback((ws: Workspace): CategoryLayouts => migrateCategoryLayouts(layoutsRef.current, ws).layouts, []);

  useEffect(() => {
    if (ui === null || !ready || !uiReady) return;
    const current = stateRef.current;
    const workspace = measureDesktop() ?? current.workspace;
    if (prevPhase.current === null) {
      const { layouts: stored, migrated } = migrateCategoryLayouts(layoutsRef.current, workspace);
      // Migração única: o que foi migrado volta ao servidor já com a versão (nunca pelo celular).
      if (!mobileRef.current) for (const m of migrated) ui.set(UI_SECTIONS.layout(m.category, m.slot), serializeLayout(m.layout));
      const change = onPhaseChange(null, phase, { layouts: stored, layout: current.layout, workspace });
      prevPhase.current = phase;
      if (change === null) return;
      categoryRef.current = change.category;
      dispatch({ type: "applyLayout", layout: change.layout, undoable: false });
      setInitialized(true);
      return;
    }
    if (prevPhase.current === phase) return;
    const change = onPhaseChange(prevPhase.current, phase, { layouts: storedLayouts(workspace), layout: current.layout, workspace });
    prevPhase.current = phase;
    if (change === null) return;
    categoryRef.current = change.category;
    dispatch({ type: "applyLayout", layout: change.layout, undoable: false });
  }, [ui, ready, uiReady, phase, dispatch, measureDesktop, storedLayouts]);

  // Volta do celular ao desktop: reaplica o arranjo salvo da categoria (o do desktop, que o
  // celular nunca grava) na área real. O arranjo em memória pode ter sido montado para a tela
  // pequena e não é salvo.
  const wasMobile = useRef(mobile);
  useEffect(() => {
    const was = wasMobile.current;
    wasMobile.current = mobile;
    if (mobile || !was || !initialized || ui === null) return;
    const workspace = measureDesktop() ?? stateRef.current.workspace;
    dispatch({ type: "applyLayout", layout: layoutForCategory(storedLayouts(workspace), categoryRef.current, workspace), undoable: false });
  }, [mobile, initialized, ui, dispatch, measureDesktop, storedLayouts]);

  // O arranjo atual é sempre salvo na categoria em vigor (nunca no celular). Só quando o arranjo
  // muda: a troca de modo sozinha não grava o arranjo em memória.
  const layout: Layout = state.layout;
  useEffect(() => {
    if (!initialized || ui === null || mobileRef.current) return;
    ui.set(UI_SECTIONS.layout(categoryRef.current, "current"), serializeLayout(layout));
  }, [layout, initialized, ui]);

  const command = useCallback(
    (cmd: LayoutCommand): { message: string; undoable: boolean } => {
      const category = categoryRef.current;
      const ws = stateRef.current.workspace;
      const all = storedLayouts(ws);
      switch (cmd) {
        case "applyMine":
          dispatch({ type: "applyLayout", layout: mineLayoutFor(all, category, ws) });
          return { message: t("layout.toast.mineApplied"), undoable: true };
        case "applyDefault":
          dispatch({ type: "applyLayout", layout: defaultLayoutFor(category, ws) });
          return { message: t("layout.toast.defaultApplied"), undoable: true };
        case "saveMine":
          ui?.set(UI_SECTIONS.layout(category, "mine"), serializeLayout(stateRef.current.layout));
          return { message: t("layout.toast.saved"), undoable: false };
        case "restoreDefault": {
          const restored = restoreDefault(all, category, ws);
          ui?.set(UI_SECTIONS.layout(category, "mine"), null);
          dispatch({ type: "applyLayout", layout: restored.layout });
          return { message: t("layout.toast.defaultRestored"), undoable: true };
        }
      }
    },
    [dispatch, ui, storedLayouts],
  );

  const restorePhaseLayout = useCallback(() => {
    dispatch({
      type: "applyLayout",
      layout: mineLayoutFor(storedLayouts(stateRef.current.workspace), categoryRef.current, stateRef.current.workspace),
    });
  }, [dispatch, storedLayouts]);

  return { layouts, category: categoryOf(phase), initialized, command, restorePhaseLayout };
}
