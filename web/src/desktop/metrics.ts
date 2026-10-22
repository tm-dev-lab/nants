/*
 * Medidas fixas do desktop (spec 094, decisão 4; spec 095, C-06): barra superior, barra de título,
 * dock e a folga do dock. O CSS (`theme/desktop.css`) declara as mesmas medidas como variáveis na
 * `.nd-root` (`--topbar-h`, `--titlebar-h`, `--dock-h`, `--dock-gap`), e a área de trabalho
 * desconta o dock inteiro. Um teste confere que o CSS e estas constantes não divergem.
 */
import type { Workspace } from "./windows";

/** Altura da barra superior (px). */
export const TOPBAR_HEIGHT = 44;
/**
 * Altura da barra de título das janelas (px, C-06). A mesma do `--titlebar-h`: o menu da janela
 * abre logo abaixo dela e a alça leste começa depois dela.
 */
export const TITLEBAR_HEIGHT = 44;
/** Botão do dock (px): 44, com ícone de 20 (095/T02, `reference/Main.dc.html`). */
export const DOCK_BUTTON = 44;
/** Altura do dock (px): o botão e o padding de 8 px (`--sp-2`) em cima e embaixo. */
export const DOCK_HEIGHT = DOCK_BUTTON + 2 * 8;
/** Folga entre o dock e a borda inferior, e entre o dock e a área de trabalho (px). */
export const DOCK_GAP = 12;
/** Menor altura de viewport em que o dock precisa caber inteiro (px). */
export const MIN_DESKTOP_HEIGHT = 600;

/** Altura reservada abaixo da área de trabalho: o dock e as duas folgas. */
export const DOCK_RESERVE = DOCK_HEIGHT + 2 * DOCK_GAP;

/** Área de trabalho para uma viewport: sem a barra superior e sem o dock (nunca negativa). */
export function workspaceForViewport(width: number, height: number): Workspace {
  return { w: Math.max(0, Math.floor(width)), h: Math.max(0, Math.floor(height) - TOPBAR_HEIGHT - DOCK_RESERVE) };
}

/** Faixa vertical ocupada pelo dock numa viewport de altura `height` (px, a partir do topo). */
export function dockBand(height: number): { top: number; bottom: number } {
  const bottom = height - DOCK_GAP;
  return { top: bottom - DOCK_HEIGHT, bottom };
}
