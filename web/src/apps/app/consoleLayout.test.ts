/*
 * Console no celular (spec 130 T06, achado da validação real a 390 px): com saída, arquivos e
 * histórico além da altura, a aba (`nd-console-pane`, só na forma compacta) encolhia a saída a zero
 * e o histórico vazava por cima do campo e do limite. O jsdom não aplica CSS: o contrato é conferido
 * de forma estática no CSS-fonte, e o efeito real pela captura do `scripts/e2e-run-modes.mjs`.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRules } from "../../desktop/test-utils";

const css = readFileSync(resolve(process.cwd(), "src/apps/app/run.css"), "utf8");
const body = (selector: string): string => cssRules(css).find((r) => r.at === null && r.selectors.includes(selector))?.body ?? "";

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
