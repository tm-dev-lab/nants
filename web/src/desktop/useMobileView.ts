/*
 * Vista do modo celular (spec 093, decisões 6 e 17) ligada ao ui-state: num espaço, a vista e a
 * seleção do celular ficam na seção `mobile` do projeto (a única que o celular grava); sem espaço
 * (boas-vindas), a vista fica só em memória.
 */
import { useCallback, useMemo, useRef, useState } from "react";
import {
  HOME,
  HOME_TABS,
  MOBILE_TABS,
  mobileViewFor,
  parseMobileState,
  serializeMobileState,
  SWITCHER,
  withSelection,
  withView,
  type MobileState,
  type MobileView,
} from "./mobile";
import type { LayoutCategory } from "./presets";
import { getSection, UI_SECTIONS } from "./uiState";
import type { UiStateHandle } from "./useUiState";

interface MobileViewApi {
  /** O que ocupa a tela agora. */
  view: MobileView;
  /** Abas da barra inferior (por categoria da fase; sem espaço, início e Alternador). */
  tabs: readonly MobileView[];
  /** Para onde vai ao fechar a janela atual. */
  closeView: MobileView;
  setView: (view: MobileView) => void;
  /** Seleção própria do celular (por cima da do desktop, só na leitura). */
  selection: Readonly<Record<string, unknown>>;
  /** Grava a seleção do celular dentro da seção `mobile`. */
  setSelection: (patch: Readonly<Record<string, unknown>>) => void;
}

/**
 * `ui` é o ui-state do projeto (`null` nas boas-vindas). `enabled` falso (desktop): nada é
 * gravado, para o desktop nunca escrever a seção do celular.
 */
export function useMobileView(ui: UiStateHandle | null, category: LayoutCategory, enabled: boolean): MobileViewApi {
  const raw = ui === null ? undefined : getSection(ui.snapshot.data, UI_SECTIONS.mobile);
  const stored = useMemo(() => parseMobileState(raw), [raw]);
  const storedRef = useRef<MobileState>(stored);
  storedRef.current = stored;
  const enabledRef = useRef(enabled);
  enabledRef.current = enabled;
  const categoryRef = useRef(category);
  categoryRef.current = category;
  const [homeView, setHomeView] = useState<MobileView>(HOME);

  const setView = useCallback(
    (view: MobileView) => {
      if (!enabledRef.current) return;
      if (ui === null) {
        setHomeView(view);
        return;
      }
      // Num espaço, "início" não existe: fechar leva ao Alternador.
      const next = view === HOME ? SWITCHER : view;
      ui.set(UI_SECTIONS.mobile, serializeMobileState(withView(storedRef.current, next, categoryRef.current)));
    },
    [ui],
  );

  const setSelection = useCallback(
    (patch: Readonly<Record<string, unknown>>) => {
      if (!enabledRef.current || ui === null) return;
      ui.set(UI_SECTIONS.mobile, serializeMobileState(withSelection(storedRef.current, patch)));
    },
    [ui],
  );

  const view = ui === null ? homeView : mobileViewFor(stored, category);
  const tabs = ui === null ? HOME_TABS : MOBILE_TABS[category];
  const closeView = ui === null ? HOME : SWITCHER;
  return useMemo(
    () => ({ view, tabs, closeView, setView, selection: stored.selection, setSelection }),
    [view, tabs, closeView, setView, stored.selection, setSelection],
  );
}
