/*
 * O que a moldura `Window` recebe do Desktop: o despacho das ações (que também move o foco e
 * anuncia), a memória de foco, a prévia de encaixe, a camada dos menus e as dependências
 * injetáveis dos testes (observador de tamanho, agendador de quadros, movimento reduzido).
 */
import { createContext, useContext } from "react";
import type { GestureMode } from "./gestures";
import type { ResizeObserverFactory } from "./useContainerSize";
import type { FrameScheduler } from "./useWindowGesture";
import type { SnapTarget, WindowAction, WindowKind } from "./windows";

export interface WindowHost {
  /** Despacha a ação do gerenciador (com foco e anúncio). */
  act: (action: WindowAction) => void;
  /** Texto para a região `role="status"` do Desktop. */
  announce: (message: string) => void;
  /** Último elemento focado dentro da janela (para restaurar o foco). */
  rememberFocus: (kind: WindowKind, element: HTMLElement) => void;
  /** Prévia translúcida do encaixe durante o arraste (`null` some). */
  snapPreview: (kind: WindowKind, target: SnapTarget | null) => void;
  /** Primeiro movimento real de um gesto (dica "Arraste até a borda para encaixar"). */
  gestureStart: (kind: WindowKind, mode: GestureMode) => void;
  /** Canto da área de trabalho no viewport. */
  workspaceOrigin: () => { x: number; y: number };
  /** Onde os menus das janelas são desenhados (fora da janela, que corta o que transborda). */
  menuLayer: HTMLElement | null;
  /** `prefers-reduced-motion`. */
  reducedMotion: () => boolean;
  createObserver?: ResizeObserverFactory | null;
  scheduler?: FrameScheduler;
  /**
   * Modo celular (decisão 17): a janela ocupa a tela inteira, sem mover, redimensionar, encaixar
   * nem menu; a barra de título só tem Fechar (alvo de 44 px).
   */
  mobile?: boolean;
}

const noop = (): void => undefined;

export const DEFAULT_WINDOW_HOST: WindowHost = {
  act: noop,
  announce: noop,
  rememberFocus: noop,
  snapPreview: noop,
  gestureStart: noop,
  workspaceOrigin: () => ({ x: 0, y: 0 }),
  menuLayer: null,
  reducedMotion: () => false,
};

export const WindowHostContext = createContext<WindowHost>(DEFAULT_WINDOW_HOST);

export function useWindowHost(): WindowHost {
  return useContext(WindowHostContext);
}
