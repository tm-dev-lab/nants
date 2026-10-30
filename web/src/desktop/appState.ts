/*
 * Estado de cada app para quem o mostra (spec 137/T02): o menu do N, as abas e o Alternador do
 * celular. Antes morava no dock, que saiu do desktop.
 * - `AppBadge`: o selo por app (pendências, "N novas"), com o número e o texto acessível;
 * - `BADGE_MAX`: o maior número desenhado no selo (o texto acessível traz o valor real);
 * - `appItemState`: fechado, aberto, minimizado ou em foco.
 */
import { isShown, topWindow, type Layout, type WindowKind } from "./windows";

export interface AppBadge {
  /** Número exibido (até `BADGE_MAX`). */
  count: number;
  /** Texto acessível (ex.: "2 pendências"). */
  label: string;
}

type AppItemState = "closed" | "open" | "minimized" | "focused";

/** Maior número desenhado no selo (o texto acessível traz o valor real). */
export const BADGE_MAX = 99;

export function appItemState(layout: Layout, kind: WindowKind, top: WindowKind | null = topWindow(layout)): AppItemState {
  const win = layout.windows[kind];
  if (win === undefined || !win.open) return "closed";
  if (win.mode === "minimized") return "minimized";
  if (isShown(win) && top === kind) return "focused";
  return "open";
}
