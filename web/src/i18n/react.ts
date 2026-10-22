/*
 * Ligação do idioma com o React (spec 101, critério 4 da T01): quem lê pelo `useT()` ou pelo
 * `useLanguage()` re-renderiza na hora em que o idioma muda, sem recarregar a página.
 */
import { useMemo, useSyncExternalStore } from "react";
import {
  formatDateIn,
  formatDateTimeIn,
  formatNumberIn,
  formatTimeIn,
  formatUsdIn,
  getLanguage,
  subscribeLanguage,
  translate,
  translatePlural,
  type Lang,
  type MessageKey,
  type MessageParams,
  type PluralBase,
} from "./index";

/** Idioma ativo, assinado. */
export function useLanguage(): Lang {
  return useSyncExternalStore(subscribeLanguage, getLanguage, getLanguage);
}

interface Translator {
  lang: Lang;
  t: (key: MessageKey, params?: MessageParams) => string;
  tp: (base: PluralBase, count: number, params?: MessageParams) => string;
  formatDate: (value: Date | number | string, options?: Intl.DateTimeFormatOptions) => string;
  formatTime: (value: Date | number | string, options?: Intl.DateTimeFormatOptions) => string;
  formatDateTime: (value: Date | number | string, options?: Intl.DateTimeFormatOptions) => string;
  formatNumber: (value: number, options?: Intl.NumberFormatOptions) => string;
  formatUsd: (value: number) => string;
}

/** Tradutor do idioma ativo; a identidade muda só quando o idioma muda. */
export function useT(): Translator {
  const lang = useLanguage();
  return useMemo<Translator>(
    () => ({
      lang,
      t: (key, params) => translate(lang, key, params),
      tp: (base, count, params) => translatePlural(lang, base, count, params),
      formatDate: (value, options) => formatDateIn(lang, value, options),
      formatTime: (value, options) => formatTimeIn(lang, value, options),
      formatDateTime: (value, options) => formatDateTimeIn(lang, value, options),
      formatNumber: (value, options) => formatNumberIn(lang, value, options),
      formatUsd: (value) => formatUsdIn(lang, value),
    }),
    [lang],
  );
}
