/*
 * Ambiente dos testes (spec 101, decisão 5 / C-05): o jsdom se apresenta em inglês (en-US), então
 * a detecção escolheria en. A suíte foi escrita em pt-BR: o idioma fica fixo em pt-BR antes de
 * cada teste, sem gravar o espelho local. Testes bilíngues usam `renderWith({ lang })`
 * (`test-utils.ts`).
 */
import { beforeEach } from "vitest";
import { setLanguage } from "./i18n";

setLanguage("pt-BR", { persist: false });

beforeEach(() => {
  setLanguage("pt-BR", { persist: false });
});
