/*
 * Spec 137/T03 (G1, G2): o dock saiu do desktop; o lugar dele é o menu do Nants (o botão N). Nenhum
 * texto de pessoa dos dicionários fala mais do dock, em nenhum idioma: a folha de atalhos, as dicas
 * e a ajuda dentro do app leem daqui. "Docker" (o pré-requisito) não conta: a busca é pela palavra.
 */
import { describe, expect, it } from "vitest";
import { DICTIONARIES } from "./index";
import { LANGS } from "./lang";

/** "dock" ou "Dock" como palavra (com plural), sem pegar "Docker". */
const DOCK_WORD = /\bdocks?\b/i;

describe("dicionários sem o dock (137/T03)", () => {
  it.each(LANGS)("nenhum valor em %s fala do dock", (lang) => {
    const hits = Object.entries(DICTIONARIES[lang]).filter(([, value]) => DOCK_WORD.test(value));
    expect(hits).toEqual([]);
  });

  it("a busca pega 'dock' e 'Dock', mas não 'Docker'", () => {
    expect(DOCK_WORD.test("Abra pelo dock")).toBe(true);
    expect(DOCK_WORD.test("Dock")).toBe(true);
    expect(DOCK_WORD.test("o Docker é pré-requisito")).toBe(false);
  });
});
