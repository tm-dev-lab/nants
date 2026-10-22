/*
 * Marca na barra superior (spec 094, decisão 4; achado 2): duas linhas no total, centradas na
 * barra de 44 px: "Nants" em cima e o slogan INTEIRO numa linha só embaixo (>= 10,5 px, tom
 * secundário). Onde não cabe, o slogan some (nunca quebra) antes de qualquer controle. O jsdom
 * não aplica CSS: as medidas e o ponto de corte são lidos no CSS-fonte.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { BRAND_NAME, BRAND_SLOGAN } from "../brand";
import { TOPBAR_HEIGHT } from "../metrics";
import { cssRuleBody, cssRules } from "../test-utils";
import { scaleVariables } from "../theme/tokens";
import { DESKTOP_MIN_WIDTH } from "../viewport";
import { BrandMark } from "./BrandMark";

afterEach(cleanup);

const read = (path: string): string => readFileSync(resolve(process.cwd(), path), "utf8");
const shellCss = read("src/desktop/theme/shell.css");
const desktopCss = read("src/desktop/theme/desktop.css");

/** Valor em px: literal ou token da escala de acabamento (094/T06), como `var(--fs-caption)`. */
const px = (body: string, property: string): number => {
  const match = new RegExp(`(?:^|;)\\s*${property}:\\s*(?:([\\d.]+)px|var\\((--[\\w-]+)\\))`).exec(body);
  if (match === null) throw new Error(`${property} ausente`);
  if (match[1] !== undefined) return Number(match[1]);
  const token = scaleVariables()[(match[2] ?? "") as `--${string}`];
  if (token === undefined) throw new Error(`${property}: token fora da escala`);
  return Number.parseFloat(token);
};

/** `max-width` (px) de uma condição `@media`, ou `null`. */
const maxWidth = (at: string | null): number | null => {
  const match = at === null ? null : /max-width:\s*([\d.]+)px/.exec(at);
  return match === null ? null : Number(match[1]);
};

describe("marca em duas linhas", () => {
  it("o slogan inteiro fica abaixo do nome, no mesmo bloco", () => {
    render(<BrandMark />);
    const name = screen.getByText(BRAND_NAME);
    const slogan = screen.getByText(BRAND_SLOGAN);
    // Nome e slogan no mesmo bloco, o slogan depois do nome.
    expect(name.parentElement).toBe(slogan.parentElement);
    expect(name.parentElement?.className).toBe("nd-brand-text");
    expect(name.nextElementSibling).toBe(slogan);
    expect(slogan.textContent).toBe(BRAND_SLOGAN);
    expect(slogan.nextElementSibling).toBeNull();
  });
});

describe("CSS da marca", () => {
  it("nome + slogan numa linha só cabem na barra, que não cresce; o slogan é legível e secundário", () => {
    const name = cssRuleBody(shellCss, ".nd-brand-name");
    const slogan = cssRuleBody(shellCss, ".nd-brand-slogan");
    // Uma linha só: nunca quebra.
    expect(slogan).toContain("white-space: nowrap");
    expect(slogan).not.toMatch(/pre-line|pre-wrap/);
    expect(px(slogan, "font-size")).toBeGreaterThanOrEqual(10.5);
    expect(px(slogan, "font-size")).toBeLessThan(px(name, "font-size"));
    expect(slogan).toContain("color: var(--muted)");
    // Duas linhas no total, com folga na barra.
    expect(px(name, "line-height") + px(slogan, "line-height")).toBeLessThanOrEqual(TOPBAR_HEIGHT - 8);
    const block = cssRuleBody(shellCss, ".nd-brand-text");
    expect(block).toContain("flex-direction: column");
    expect(block).toContain("justify-content: center");
    expect(block).toContain("align-self: center");
    expect(cssRuleBody(shellCss, ".nd-brand")).toContain("align-items: center");
    expect(cssRuleBody(desktopCss, ".nd-topbar")).toContain("height: var(--topbar-h)");
  });

  it("estreita: o slogan some (continua para leitores de tela) antes de qualquer controle", () => {
    const rules = [...cssRules(shellCss), ...cssRules(desktopCss)];
    const hide = rules.filter((r) => r.selectors.includes(".nd-brand-slogan") && r.at !== null);
    expect(hide).toHaveLength(1);
    const cut = maxWidth(hide[0]?.at ?? null);
    if (cut === null) throw new Error("ponto de corte ausente");
    expect(hide[0]?.body).toContain("clip: rect(0, 0, 0, 0)");
    expect(hide[0]?.body).not.toContain("display: none");
    // Abaixo do corte ainda é desktop (o celular começa em 1024): há uma faixa sem slogan e com tudo.
    expect(cut).toBeGreaterThanOrEqual(DESKTOP_MIN_WIDTH);
    // Nenhuma outra regra de largura mexe na barra antes do slogan (corte maior ou igual).
    const others = rules.filter(
      (r) =>
        !r.selectors.includes(".nd-brand-slogan") &&
        (maxWidth(r.at) ?? 0) > cut &&
        r.selectors.some((s) => /nd-topbar|nd-bar-|nd-brand|nd-run-pill|nd-space/.test(s)),
    );
    expect(others).toEqual([]);
  });
});
