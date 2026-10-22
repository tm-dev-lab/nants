/*
 * Idiomas suportados e detecção (spec 101, decisão 2).
 *
 * Ordem de escolha:
 * 1. a preferência salva;
 * 2. `navigator.languages`, na ordem: o primeiro item cujo idioma principal é `pt` (pt-BR) ou
 *    `en` (en) decide;
 * 3. nenhum casou: en (o produto é para o mundo; pt-BR só por detecção).
 */

export type Lang = "pt-BR" | "en";

export const LANGS: readonly Lang[] = ["pt-BR", "en"];

/** Idioma quando nada casa. */
const FALLBACK_LANG: Lang = "en";

export function isLang(value: unknown): value is Lang {
  return typeof value === "string" && (LANGS as readonly string[]).includes(value);
}

/** Idioma suportado de uma etiqueta BCP 47 (`pt-PT` -> pt-BR, `en-GB` -> en), ou `null`. */
export function langFromTag(tag: string): Lang | null {
  const primary = tag.trim().toLowerCase().split(/[-_]/)[0] ?? "";
  if (primary === "pt") return "pt-BR";
  if (primary === "en") return "en";
  return null;
}

/** Aplica a ordem de escolha: preferência salva, depois `languages` na ordem, depois en. */
export function detectLanguage(saved: unknown, languages: readonly string[] | null | undefined): Lang {
  if (isLang(saved)) return saved;
  for (const tag of languages ?? []) {
    if (typeof tag !== "string") continue;
    const lang = langFromTag(tag);
    if (lang !== null) return lang;
  }
  return FALLBACK_LANG;
}

/** Valor da seção global `language` do estado da interface: `{lang}`. */
interface LanguageState {
  lang: Lang;
}

/** Preferência válida da seção `language`, ou `null` (sem preferência salva). */
export function parseLanguageState(value: unknown): Lang | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const lang: unknown = (value as Record<string, unknown>).lang;
  return isLang(lang) ? lang : null;
}

export function languageStateValue(lang: Lang): LanguageState {
  return { lang };
}
