// @vitest-environment node
/*
 * Dock inteiro na tela (spec 094, decisão 4): a área de trabalho desconta o dock e a `.nd-root`
 * usa a altura real da viewport. O jsdom não aplica CSS: confere-se a regra no CSS-fonte e o
 * cálculo da área para toda altura a partir de 600 px.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { DOCK_BUTTON, DOCK_GAP, DOCK_HEIGHT, DOCK_RESERVE, dockBand, MIN_DESKTOP_HEIGHT, TITLEBAR_HEIGHT, TOPBAR_HEIGHT, workspaceForViewport } from "./metrics";
import { cssRuleBody } from "./test-utils";
import { MIN_WINDOW_H } from "./windows";

const css = readFileSync(resolve(process.cwd(), "src/desktop/theme/desktop.css"), "utf8");

/** Valor de uma declaração no corpo de uma regra (a última, como no navegador). */
function declarations(body: string, property: string): string[] {
  return body
    .split(";")
    .map((d) => d.trim())
    .filter((d) => d.startsWith(`${property}:`))
    .map((d) => d.slice(property.length + 1).trim());
}

describe("regras de CSS do dock e da área de trabalho", () => {
  const root = cssRuleBody(css, ".nd-root");

  it("a raiz declara as mesmas medidas de metrics.ts", () => {
    expect(declarations(root, "--topbar-h")).toEqual([`${TOPBAR_HEIGHT}px`]);
    expect(declarations(root, "--titlebar-h")).toEqual([`${TITLEBAR_HEIGHT}px`]);
    expect(declarations(root, "--dock-h")).toEqual([`${DOCK_HEIGHT}px`]);
    expect(declarations(root, "--dock-gap")).toEqual([`${DOCK_GAP}px`]);
  });

  it("a raiz usa a altura real da viewport (100dvh, ou --app-height) e não rola", () => {
    const heights = declarations(root, "height");
    expect(heights.at(-1)).toBe("var(--app-height, 100dvh)");
    expect(heights).toContain("100dvh");
    expect(declarations(root, "overflow").at(-1)).toBe("clip");
  });

  it("a área de trabalho desconta a barra, o dock e as duas folgas; o dock usa as mesmas medidas", () => {
    const work = cssRuleBody(css, ".nd-workarea");
    expect(declarations(work, "top")).toEqual(["var(--topbar-h)"]);
    expect(declarations(work, "bottom")).toEqual(["calc(var(--dock-h) + 2 * var(--dock-gap))"]);
    const dock = cssRuleBody(css, ".nd-dock");
    expect(declarations(dock, "bottom")).toEqual(["var(--dock-gap)"]);
    expect(declarations(dock, "height")).toEqual(["var(--dock-h)"]);
    expect(declarations(cssRuleBody(css, ".nd-topbar"), "height")).toEqual(["var(--topbar-h)"]);
  });
});

describe("cálculo da área e do dock", () => {
  it("de 600 px para cima, o dock fica inteiro na tela e abaixo da área de trabalho", () => {
    for (let height = MIN_DESKTOP_HEIGHT; height <= 1600; height += 1) {
      const ws = workspaceForViewport(1440, height);
      const band = dockBand(height);
      expect(band.top).toBeGreaterThanOrEqual(0);
      expect(band.bottom).toBeLessThanOrEqual(height);
      // A área de trabalho termina antes do dock (com a folga).
      expect(TOPBAR_HEIGHT + ws.h + DOCK_GAP).toBeLessThanOrEqual(band.top);
      // Cabe ao menos uma janela de altura mínima.
      expect(ws.h).toBeGreaterThanOrEqual(MIN_WINDOW_H);
    }
  });

  it("860 px de altura (achado 9): dock de 60 px (botão de 44, 095/T02) de 788 a 848", () => {
    expect(workspaceForViewport(1440, 860)).toEqual({ w: 1440, h: 860 - TOPBAR_HEIGHT - DOCK_RESERVE });
    expect(DOCK_HEIGHT).toBe(DOCK_BUTTON + 2 * 8);
    expect(dockBand(860)).toEqual({ top: 788, bottom: 848 });
  });

  it("nunca negativa", () => {
    expect(workspaceForViewport(-10, 50)).toEqual({ w: 0, h: 0 });
  });
});
