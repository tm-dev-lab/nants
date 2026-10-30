/*
 * Layout do Console e das formas da janela App (spec 130 T06 e 137/T04). O jsdom não aplica CSS: o
 * contrato é conferido de forma estática no CSS-fonte, e o efeito real pelas capturas no navegador
 * (`scripts/e2e-run-modes.mjs` e as da 137/T04, em duas larguras e nos dois temas).
 *
 * - Celular (130 T06, achado da validação real a 390 px): com saída, arquivos e histórico além da
 *   altura, a aba (`nd-console-pane`) encolhia a saída a zero e o histórico vazava por cima do campo.
 * - Desktop (137/T04, achados do teste da rc.20): as colunas não rolavam e a faixa delas tinha
 *   `min-height: 0`. O "Como usar" e as provas da coluna do lado passavam por cima da dica do rodapé;
 *   com a janela baixa, a saída encolhia abaixo dos 12rem da caixa e o registro do contêiner
 *   transbordava por cima do campo, do limite e do fim do painel.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRules } from "../../desktop/test-utils";

const css = readFileSync(resolve(process.cwd(), "src/apps/app/run.css"), "utf8");
const rules = cssRules(css).filter((r) => r.at === null);
const body = (selector: string): string => rules.find((r) => r.selectors.includes(selector))?.body ?? "";

describe("Console compacto (celular)", () => {
  it("a aba rola por inteiro em vez de vazar por cima do campo", () => {
    const pane = body(".nd-console-pane");
    expect(pane).toMatch(/flex: 1;/);
    expect(pane).toContain("min-height: 0");
    expect(pane).toContain("overflow-y: auto");
  });

  it("nada dentro da aba encolhe, e a saída mantém a altura mínima com rolagem própria", () => {
    expect(body(".nd-console-pane > *")).toContain("flex-shrink: 0");
    expect(body(".nd-console-pane > .nd-console-out")).toContain("flex: 1 0 auto");
    expect(body(".nd-console-pane > .nd-console-out .nd-scroll-wrap")).toContain("flex: 1 0 12rem");
  });

  it("o campo e o limite seguem fora da aba e não encolhem", () => {
    expect(body(".nd-console-form")).toContain("flex-shrink: 0");
    expect(body(".nd-console-limits")).toContain("flex-shrink: 0");
  });
});

describe("colunas do Console e do Terminou (desktop, 137/T04)", () => {
  it("a faixa das colunas ocupa o que sobra, com um piso: abaixo dele a página rola, sem nada por cima", () => {
    const split = body(".nd-runview-split");
    expect(split).toContain("flex: 1 1 0;");
    expect(split).toContain("--runview-split-min: 20rem;");
    expect(split).toContain("min-height: var(--runview-split-min);");
    expect(split).not.toContain("min-height: 0");
  });

  it("as duas colunas rolam por dentro e nunca pintam fora da faixa", () => {
    const both = body(".nd-runview-main");
    expect(rules.find((r) => r.selectors.includes(".nd-runview-main"))?.selectors).toContain(".nd-runview-side");
    expect(both).toContain("min-height: 0;");
    expect(both).toContain("min-width: 0;");
    expect(both).toContain("overflow-y: auto;");
    expect(both).toContain("overflow-x: hidden;");
    // A folga do anel de foco: o padding devolvido pela margem negativa, os dois do mesmo token.
    expect(both).toContain("padding: var(--sp-1);");
    expect(both).toContain("margin: calc(var(--sp-1) * -1);");
  });

  it("na coluna do lado os cartões ficam inteiros (quem rola é a coluna)", () => {
    expect(body(".nd-runview-side > *")).toContain("flex-shrink: 0;");
  });

  it("a saída do Console não encolhe abaixo da caixa do registro, e o registro rola dentro da caixa", () => {
    const out = body(".nd-console-out");
    expect(out).toContain("min-height: auto;");
    expect(out).not.toContain("min-height: 0");
    const box = body(".nd-console-out .nd-scroll-wrap");
    expect(box).toContain("min-height: 12rem;");
    // A altura da caixa vem da coluna, nunca do texto do registro.
    expect(box).toContain("contain: size;");
    expect(body(".nd-runview-main .nd-scroll-wrap")).toContain("contain: size;");
  });

  it("estreita: empilha e a página rola; as colunas não rolam nem encolhem", () => {
    const split = body('[data-size="narrow"] .nd-runview-split');
    expect(split).toContain("flex-direction: column;");
    expect(split).toContain("flex: 1 0 auto;");
    const cols = body('[data-size="narrow"] .nd-runview-main');
    expect(rules.find((r) => r.selectors.includes('[data-size="narrow"] .nd-runview-main'))?.selectors).toContain('[data-size="narrow"] .nd-runview-side');
    expect(cols).toContain("flex: 0 0 auto;");
    expect(cols).toContain("overflow: visible;");
  });

  it("nenhum texto alarga a janela: o painel quebra palavras longas (caminho, comando, URL, log)", () => {
    expect(body(".nd-runview")).toContain("overflow-wrap: anywhere;");
  });

  it("o rodapé (a dica do Console) vem depois da faixa e não encolhe", () => {
    expect(body(".nd-runview-foot")).toContain("flex-shrink: 0;");
  });
});
