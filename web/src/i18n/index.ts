/*
 * Base de idiomas (spec 101): dicionário próprio, sem biblioteca externa.
 *
 * - `t(key, params)`: texto no idioma ativo, com parâmetros nomeados (`{name}`);
 * - `tp(base, count, params)`: plural simples (`base.one`/`base.other`) por `Intl.PluralRules`;
 * - formatadores `Intl` (data, hora, número, US$) no idioma ativo;
 * - idioma ativo num store mínimo (assinatura), para a troca valer na hora; os componentes leem
 *   com `useT()`/`useLanguage()` (`./react`) e re-renderizam sozinhos.
 *
 * O armazenamento local do navegador (`nants.lang`) é só o espelho para a primeira pintura não piscar; a preferência
 * de verdade fica na seção global `language` do estado da interface (ver `./uiStateBinding`).
 */
// Sem o cliente da API nem o ui-state: o i18n não entra em ciclo com eles (spec 134 T06).
import { setRequestLanguage } from "../api/requestLanguage";
import { defaultStorage } from "../desktop/browserStorage";
import { en } from "./en";
import { detectLanguage, type Lang } from "./lang";
import { ptBR, type MessageKey } from "./ptBR";

export type { Lang } from "./lang";
export type { MessageKey } from "./ptBR";

export const DICTIONARIES: Readonly<Record<Lang, Readonly<Record<MessageKey, string>>>> = {
  "pt-BR": ptBR,
  en,
};

/** Chave do espelho local da preferência. */
export const LANG_STORAGE_KEY = "nants.lang";

export type MessageParams = Readonly<Record<string, string | number>>;

/** Bases com as duas formas (`.one` e `.other`) no dicionário. */
type PluralBaseOf<K> = K extends `${infer B}.one` ? (`${B}.other` extends MessageKey ? B : never) : never;
export type PluralBase = PluralBaseOf<MessageKey>;

// --- store do idioma ativo ---

type StorageLike = Pick<Storage, "getItem" | "setItem">;

/** Preferência espelhada no armazenamento local (ou `null`). */
export function readStoredLanguage(storage: StorageLike | null = defaultStorage()): string | null {
  if (storage === null) return null;
  try {
    return storage.getItem(LANG_STORAGE_KEY);
  } catch {
    return null;
  }
}

function navigatorLanguages(): readonly string[] {
  if (typeof navigator === "undefined") return [];
  if (Array.isArray(navigator.languages) && navigator.languages.length > 0) return navigator.languages;
  return typeof navigator.language === "string" && navigator.language.length > 0 ? [navigator.language] : [];
}

let active: Lang = detectLanguage(readStoredLanguage(), navigatorLanguages());
// O servidor recebe o idioma ativo a cada requisição (spec 102): os agentes escrevem nele.
setRequestLanguage(active);
const listeners = new Set<() => void>();

export function getLanguage(): Lang {
  return active;
}

export function subscribeLanguage(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function applyDocumentLang(lang: Lang): void {
  if (typeof document !== "undefined" && document.documentElement !== null) document.documentElement.lang = lang;
}

export interface SetLanguageOptions {
  /** Grava o espelho local (padrão: sim). */
  persist?: boolean;
  storage?: StorageLike | null;
}

/** Troca o idioma ativo: `<html lang>`, espelho local e aviso aos assinantes (sem recarregar). */
export function setLanguage(lang: Lang, options: SetLanguageOptions = {}): void {
  applyDocumentLang(lang);
  setRequestLanguage(lang);
  if (options.persist ?? true) {
    const storage = options.storage === undefined ? defaultStorage() : options.storage;
    try {
      storage?.setItem(LANG_STORAGE_KEY, lang);
    } catch {
      // Cota cheia ou modo privado: o espelho é opcional.
    }
  }
  if (lang === active) return;
  active = lang;
  for (const listener of [...listeners]) listener();
}

/**
 * Primeira pintura (`main.tsx`, antes do render): escolhe pela preferência espelhada e pelo
 * navegador e aplica `<html lang>`. O estado do servidor chega depois e vence.
 */
export function initLanguage(
  storage: StorageLike | null = defaultStorage(),
  languages: readonly string[] = navigatorLanguages(),
): Lang {
  const lang = detectLanguage(readStoredLanguage(storage), languages);
  setLanguage(lang, { persist: false });
  return lang;
}

// --- tradução ---

const PARAM_RE = /\{(\w+)\}/g;

function interpolate(template: string, params: MessageParams | undefined): string {
  if (params === undefined) return template;
  return template.replace(PARAM_RE, (whole, name: string) => {
    const value = Object.hasOwn(params, name) ? params[name] : undefined;
    return value === undefined ? whole : String(value);
  });
}

/** Texto de `key` em `lang` (sem cair para outro idioma: a paridade é garantida pelo tipo). */
export function translate(lang: Lang, key: MessageKey, params?: MessageParams): string {
  return interpolate(DICTIONARIES[lang][key], params);
}

/** Texto de `key` no idioma ativo. */
export function t(key: MessageKey, params?: MessageParams): string {
  return translate(active, key, params);
}

const pluralRulesCache = new Map<Lang, Intl.PluralRules>();

function pluralRules(lang: Lang): Intl.PluralRules {
  let rules = pluralRulesCache.get(lang);
  if (rules === undefined) {
    rules = new Intl.PluralRules(lang);
    pluralRulesCache.set(lang, rules);
  }
  return rules;
}

/**
 * Forma do plural: categoria `one` do `Intl.PluralRules` usa `.one`; as demais, `.other`.
 * Exceção (decisão do Arquiteto, 101/T01): em pt-BR o zero usa o plural ("0 avisos"), embora o
 * `Intl.PluralRules` trate 0 como singular em português.
 */
function pluralForm(lang: Lang, count: number): "one" | "other" {
  if (lang === "pt-BR" && count === 0) return "other";
  return pluralRules(lang).select(count) === "one" ? "one" : "other";
}

/** Plural simples em `lang`: `base.one` ou `base.other` (ver `pluralForm`). `{count}` já vem preenchido. */
export function translatePlural(lang: Lang, base: PluralBase, count: number, params?: MessageParams): string {
  const form = pluralForm(lang, count);
  const key = `${base}.${form}` as MessageKey;
  return translate(lang, key, { count: formatNumberIn(lang, count), ...params });
}

/**
 * Tabela de rótulos que acompanha o idioma: cada leitura (`labels[id]`) devolve o texto no idioma
 * ativo. Mantém a forma das tabelas antigas (`Record<id, string>`) para quem já as usa; o
 * componente que a lê assina o idioma (`useT()`/`useLanguage()`) para re-renderizar na troca.
 */
export function localized<K extends string>(keys: Readonly<Record<K, MessageKey>>): Readonly<Record<K, string>> {
  const out = {} as Record<K, string>;
  for (const id of Object.keys(keys) as K[]) {
    Object.defineProperty(out, id, { enumerable: true, get: () => t(keys[id]) });
  }
  return out;
}

/** Plural simples no idioma ativo. */
export function tp(base: PluralBase, count: number, params?: MessageParams): string {
  return translatePlural(active, base, count, params);
}

// --- formatadores Intl ---

type DateInput = Date | number | string;

function toDate(value: DateInput): Date {
  return value instanceof Date ? value : new Date(value);
}

export function formatDateIn(lang: Lang, value: DateInput, options: Intl.DateTimeFormatOptions = { dateStyle: "short" }): string {
  return new Intl.DateTimeFormat(lang, options).format(toDate(value));
}

export function formatTimeIn(lang: Lang, value: DateInput, options: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit" }): string {
  return new Intl.DateTimeFormat(lang, options).format(toDate(value));
}

export function formatDateTimeIn(
  lang: Lang,
  value: DateInput,
  options: Intl.DateTimeFormatOptions = { dateStyle: "short", timeStyle: "short" },
): string {
  return new Intl.DateTimeFormat(lang, options).format(toDate(value));
}

export function formatNumberIn(lang: Lang, value: number, options?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(lang, options).format(value);
}

/**
 * Valor em dólar: sempre "US$" (não há conversão de moeda), com o número no idioma ativo.
 * Valores pequenos (modelos baratos) não podem virar "US$ 0,00": mostram 4 casas.
 */
export function formatUsdIn(lang: Lang, value: number): string {
  const digits = value > 0 && value < 0.01 ? 4 : 2;
  return `US$ ${formatNumberIn(lang, value, { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

export function formatDate(value: DateInput, options?: Intl.DateTimeFormatOptions): string {
  return formatDateIn(active, value, options);
}

export function formatTime(value: DateInput, options?: Intl.DateTimeFormatOptions): string {
  return formatTimeIn(active, value, options);
}

export function formatDateTime(value: DateInput, options?: Intl.DateTimeFormatOptions): string {
  return formatDateTimeIn(active, value, options);
}

export function formatNumber(value: number, options?: Intl.NumberFormatOptions): string {
  return formatNumberIn(active, value, options);
}

export function formatUsd(value: number): string {
  return formatUsdIn(active, value);
}
