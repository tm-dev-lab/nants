/*
 * Aparência escolhida pelo usuário (seção global `appearance` do ui-state): tema Escuro, Claro
 * ou Automático (`prefers-color-scheme`) e o papel de parede. Valores inválidos voltam ao padrão.
 */
import { useSyncExternalStore } from "react";
import { THEME_PREFERENCES, THEMES, WALLPAPER_IDS, type ThemeName, type ThemePreference, type WallpaperId } from "./tokens";

export interface Appearance {
  theme: ThemePreference;
  wallpaper: WallpaperId;
}

export const DEFAULT_APPEARANCE: Readonly<Appearance> = { theme: "dark", wallpaper: "dots" };

/** Valida o valor cru da seção; campos inválidos ou ausentes ficam no padrão. */
export function parseAppearance(value: unknown): Appearance {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return { ...DEFAULT_APPEARANCE };
  const raw = value as Record<string, unknown>;
  const theme = (THEME_PREFERENCES as readonly unknown[]).includes(raw.theme)
    ? (raw.theme as ThemePreference)
    : DEFAULT_APPEARANCE.theme;
  const wallpaper = (WALLPAPER_IDS as readonly unknown[]).includes(raw.wallpaper)
    ? (raw.wallpaper as WallpaperId)
    : DEFAULT_APPEARANCE.wallpaper;
  return { theme, wallpaper };
}

/** Tema efetivo: o Automático segue o sistema. */
export function resolveTheme(preference: ThemePreference, systemDark: boolean): ThemeName {
  if (preference === "auto") return systemDark ? "dark" : "light";
  return preference;
}

const DARK_QUERY = "(prefers-color-scheme: dark)";

function darkQuery(): MediaQueryList | null {
  return typeof window !== "undefined" && typeof window.matchMedia === "function" ? window.matchMedia(DARK_QUERY) : null;
}

function subscribeSystemDark(listener: () => void): () => void {
  const query = darkQuery();
  if (query === null) return () => undefined;
  query.addEventListener("change", listener);
  return () => query.removeEventListener("change", listener);
}

/** O sistema prefere o escuro (sem `matchMedia`: sim, a identidade padrão do Nants). */
function useSystemDark(): boolean {
  return useSyncExternalStore(
    subscribeSystemDark,
    () => darkQuery()?.matches ?? true,
    () => true,
  );
}

/** Tema efetivo da preferência, reagindo à mudança do sistema no modo Automático. */
export function useResolvedTheme(preference: ThemePreference): ThemeName {
  const systemDark = useSystemDark();
  return resolveTheme(preference, systemDark);
}

/** Grava `<meta name=... content=...>` (cria a meta se faltar). */
function setMeta(doc: Document, name: string, content: string): void {
  let meta = doc.head.querySelector<HTMLMetaElement>(`meta[name="${name}"]`);
  if (meta === null) {
    meta = doc.createElement("meta");
    meta.name = name;
    doc.head.appendChild(meta);
  }
  if (meta.content !== content) meta.content = content;
}

/**
 * Metas do navegador conforme o tema efetivo: `theme-color` (barra do navegador no celular) com
 * o fundo do tema e `color-scheme` (controles nativos). As cores vêm dos tokens.
 */
export function applyThemeMeta(theme: ThemeName, doc: Document = document): void {
  setMeta(doc, "theme-color", THEMES[theme].bg);
  setMeta(doc, "color-scheme", theme);
}
