// @vitest-environment node
/*
 * Área de trabalho sem dock (spec 137/T02; antes, spec 094, decisão 4): a área vai da barra de cima
 * até a borda de baixo e a `.nd-root` usa a altura real da viewport. O jsdom não aplica CSS:
 * confere-se a regra no CSS-fonte e o cálculo da área para toda altura a partir de 600 px.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { LEGACY_DOCK_RESERVE, MIN_DESKTOP_HEIGHT, TITLEBAR_HEIGHT, TOPBAR_HEIGHT, workspaceForViewport } from "./metrics";
import { cssRuleBody } from "./test-utils";
import { MIN_WINDOW_H } from "./windows";

const css = readFileSync(resolve(process.cwd(), "src/desktop/theme/desktop.css"), "utf8");
const shellCss = readFileSync(resolve(process.cwd(), "src/desktop/theme/shell.css"), "utf8");
const appsCss = readFileSync(resolve(process.cwd(), "src/apps/shared/apps.css"), "utf8");

/** Valor de uma declaração no corpo de uma regra (a última, como no navegador). */
function declarations(body: string, property: string): string[] {
  return body
    .split(";")
    .map((d) => d.trim())
    .filter((d) => d.startsWith(`${property}:`))
    .map((d) => d.slice(property.length + 1).trim());
}

describe("regras de CSS da área de trabalho", () => {
  const root = cssRuleBody(css, ".nd-root");

  it("a raiz declara as mesmas medidas de metrics.ts, sem as do dock", () => {
    expect(declarations(root, "--topbar-h")).toEqual([`${TOPBAR_HEIGHT}px`]);
    expect(declarations(root, "--titlebar-h")).toEqual([`${TITLEBAR_HEIGHT}px`]);
    for (const file of [css, shellCss, appsCss]) {
      expect(file).not.toContain("--dock-h");
      expect(file).not.toContain("--dock-gap");
      expect(file).not.toMatch(/\.nd-dock\b/);
    }
  });

  it("a raiz usa a altura real da viewport (100dvh, ou --app-height) e não rola", () => {
    const heights = declarations(root, "height");
    expect(heights.at(-1)).toBe("var(--app-height, 100dvh)");
    expect(heights).toContain("100dvh");
    expect(declarations(root, "overflow").at(-1)).toBe("clip");
  });

  it("a área de trabalho vai da barra até a borda de baixo, com as laterais também em 0", () => {
    const work = cssRuleBody(css, ".nd-workarea");
    expect(declarations(work, "top")).toEqual(["var(--topbar-h)"]);
    expect(declarations(work, "bottom")).toEqual(["0"]);
    expect(declarations(work, "left")).toEqual(["0"]);
    expect(declarations(work, "right")).toEqual(["0"]);
    expect(declarations(cssRuleBody(css, ".nd-topbar"), "height")).toEqual(["var(--topbar-h)"]);
  });

  it("dica, aviso do shell e avisos de permissão se baseiam na borda de baixo (sem o 104 px do dock)", () => {
    expect(declarations(root, "--float-bottom")).toEqual(["var(--sp-5)"]);
    expect(declarations(cssRuleBody(css, ".nd-hint"), "bottom")).toEqual(["var(--float-bottom)"]);
    expect(declarations(cssRuleBody(shellCss, ".nd-toast"), "bottom")).toEqual(["var(--float-bottom)"]);
    expect(declarations(cssRuleBody(appsCss, ".nd-permission-toasts"), "bottom")).toEqual(["var(--float-bottom)"]);
    for (const file of [css, shellCss, appsCss]) expect(file).not.toContain("bottom: 104px");
  });

  it("com o menu do N aberto, a dica fica abaixo da camada do menu (não aparece sobre o véu)", () => {
    const hint = Number(declarations(cssRuleBody(css, ".nd-hint"), "z-index")[0]);
    const menu = Number(declarations(cssRuleBody(css, ".nd-nmenu-layer"), "z-index")[0]);
    expect(Number.isFinite(hint) && Number.isFinite(menu)).toBe(true);
    expect(hint).toBeLessThan(menu);
  });
});

describe("cálculo da área", () => {
  it("de 600 px para cima, a área é a viewport sem a barra de cima e cabe uma janela de altura mínima", () => {
    for (let height = MIN_DESKTOP_HEIGHT; height <= 1600; height += 1) {
      const ws = workspaceForViewport(1440, height);
      expect(TOPBAR_HEIGHT + ws.h).toBe(height);
      expect(ws.h).toBeGreaterThanOrEqual(MIN_WINDOW_H);
    }
  });

  it("860 px de altura: 816 px de área, os 84 px do dock antigo devolvidos às janelas", () => {
    expect(workspaceForViewport(1440, 860)).toEqual({ w: 1440, h: 816 });
    expect(LEGACY_DOCK_RESERVE).toBe(84);
  });

  it("nunca negativa", () => {
    expect(workspaceForViewport(-10, 20)).toEqual({ w: 0, h: 0 });
  });
});
