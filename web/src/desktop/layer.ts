/*
 * Camada de sobreposição do desktop: menus da barra superior, lançador, diálogos e avisos são
 * desenhados aqui (dentro de `.nd-root`, onde valem as variáveis do tema), fora da barra superior
 * (o vidro dela viraria o bloco de contenção e cortaria o que transborda).
 */
import { createContext, useContext } from "react";

export const DesktopLayerContext = createContext<HTMLElement | null>(null);

/** Elemento da camada de sobreposição (`null` até montar). */
export function useDesktopLayer(): HTMLElement | null {
  return useContext(DesktopLayerContext);
}
