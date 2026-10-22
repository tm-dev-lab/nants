/*
 * Preferência de idioma no estado da interface por servidor (spec 101, decisão 2): seção global
 * `language` (`{lang}`), como o `network` da 097, para valer em qualquer navegador do computador.
 *
 * - Ao carregar, o servidor vence: assim que o cliente global tem resposta do servidor (`ready`) e
 *   a seção traz um idioma válido, ele vira o ativo e sobrescreve o espelho local.
 * - Sem preferência no servidor, fica o idioma detectado (nada é gravado).
 * - Outra aba que troca o idioma chega pelo mesmo cliente (BroadcastChannel) e vale aqui também.
 * - `chooseLanguage` é a troca feita pela pessoa: vale na hora e vai para o servidor.
 */
import { UI_SECTIONS, type UiStateSnapshot } from "../desktop/uiState";
import { setLanguage, type SetLanguageOptions } from "./index";
import { languageStateValue, parseLanguageState, type Lang } from "./lang";

/** O que a ligação precisa do cliente global (`UiStateSync`). */
interface LanguageStateSource {
  subscribe: (listener: () => void) => () => void;
  getSnapshot: () => UiStateSnapshot;
  get: (section: string) => unknown;
}

interface LanguageStateSink {
  set: (section: string, value: unknown) => boolean;
}

/** Segue a seção `language` do cliente global. Devolve a função que desliga. */
export function bindLanguageToUiState(source: LanguageStateSource, options: SetLanguageOptions = {}): () => void {
  let lastApplied: Lang | null = null;
  const apply = (): void => {
    if (!source.getSnapshot().ready) return;
    const lang = parseLanguageState(source.get(UI_SECTIONS.language));
    if (lang === null || lang === lastApplied) return;
    lastApplied = lang;
    setLanguage(lang, options);
  };
  apply();
  return source.subscribe(apply);
}

/** Troca feita pela pessoa: vale na hora (sem recarregar) e fica salva no servidor. */
export function chooseLanguage(lang: Lang, sink: LanguageStateSink, options: SetLanguageOptions = {}): void {
  setLanguage(lang, options);
  sink.set(UI_SECTIONS.language, languageStateValue(lang));
}

