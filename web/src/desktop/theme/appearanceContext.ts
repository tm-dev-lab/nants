/*
 * Aparência atual para quem está dentro do Desktop (spec 093, decisão 13): a escolha salva
 * (`appearance`), o tema efetivo e a gravação. Fornecida pelo Desktop; usada pela aba Aparência
 * das Configurações (T08, igual ao Personalizar) e pelo tema do CodeMirror dos Arquivos.
 * Spec 101: também a troca de idioma (Configurações, Aparência), gravada na seção global `language`.
 */
import { createContext, useContext } from "react";
import type { Lang } from "../../i18n";
import { DEFAULT_APPEARANCE, type Appearance } from "./appearance";
import type { ThemeName } from "./tokens";

export interface AppearanceApi {
  appearance: Appearance;
  /** Tema efetivo (o Automático já resolvido). */
  theme: ThemeName;
  setAppearance: (next: Appearance) => void;
  /** Troca o idioma na hora e grava a preferência no servidor (spec 101, decisão 3). */
  setLanguage: (lang: Lang) => void;
}

const noop = (): void => undefined;

export const AppearanceContext = createContext<AppearanceApi>({ appearance: DEFAULT_APPEARANCE, theme: "dark", setAppearance: noop, setLanguage: noop });

export function useAppearance(): AppearanceApi {
  return useContext(AppearanceContext);
}
