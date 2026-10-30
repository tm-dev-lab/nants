/*
 * Medidas fixas do desktop (spec 094, decisão 4; spec 095, C-06): barra superior e barra de título.
 * O CSS (`theme/desktop.css`) declara as mesmas medidas como variáveis na `.nd-root`
 * (`--topbar-h`, `--titlebar-h`). Um teste confere que o CSS e estas constantes não divergem.
 *
 * Sem dock (spec 137/T02): a área de trabalho vai da barra de cima até a borda de baixo. A margem
 * em volta das janelas é a mesma nos quatro lados (a folga dos presets, `PRESET_GAP`).
 */
import type { Workspace } from "./windows";

/** Altura da barra superior (px). */
export const TOPBAR_HEIGHT = 44;
/**
 * Altura da barra de título das janelas (px, C-06). A mesma do `--titlebar-h`: o menu da janela
 * abre logo abaixo dela e a alça leste começa depois dela.
 */
export const TITLEBAR_HEIGHT = 44;
/** Menor altura de viewport do modo desktop (px). */
export const MIN_DESKTOP_HEIGHT = 600;

/**
 * Altura que o dock reservava embaixo da área de trabalho até a spec 137 (o dock de 60 px e as
 * duas folgas de 12). Só a migração dos layouts salvos antes da 137 a usa (`presets.ts`).
 */
export const LEGACY_DOCK_RESERVE = 84;

/** Área de trabalho para uma viewport: sem a barra superior (nunca negativa). */
export function workspaceForViewport(width: number, height: number): Workspace {
  return { w: Math.max(0, Math.floor(width)), h: Math.max(0, Math.floor(height) - TOPBAR_HEIGHT) };
}
