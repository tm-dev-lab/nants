/*
 * Visibilidade real de uma janela (spec 093, decisão 4, desempenho): uma janela minimizada,
 * fechada, coberta por uma maximizada à frente ou com a aba oculta não está visível, e as
 * janelas pausam os seus polls (Arquivos, logs do App, atividade).
 */
import { createContext, useContext, useSyncExternalStore } from "react";
import { isCoveredByMaximized, isShown, type Layout, type WindowKind } from "./windows";

/** Arranjo do espaço ativo, fornecido pelo Desktop (T04). */
export const WindowLayoutContext = createContext<Layout | null>(null);

/** Regra pura: aberta, não minimizada, não coberta por maximizada e aba visível. */
export function isWindowVisible(layout: Layout | null, kind: WindowKind, pageVisible: boolean): boolean {
  if (!pageVisible || layout === null) return false;
  return isShown(layout.windows[kind]) && !isCoveredByMaximized(layout, kind);
}

function subscribeVisibility(listener: () => void): () => void {
  document.addEventListener("visibilitychange", listener);
  return () => document.removeEventListener("visibilitychange", listener);
}

/** A aba está visível (`document.hidden` falso). */
export function usePageVisible(): boolean {
  return useSyncExternalStore(
    subscribeVisibility,
    () => !document.hidden,
    () => true,
  );
}

/** `true` enquanto a janela `kind` está de fato visível para o usuário. */
export function useWindowVisibility(kind: WindowKind): boolean {
  const layout = useContext(WindowLayoutContext);
  const pageVisible = usePageVisible();
  return isWindowVisible(layout, kind, pageVisible);
}
