// @vitest-environment node
/*
 * Controle segmentado no titânio (131/T07, U3 do Test 9). No celular e no tema claro, o segmento
 * escolhido ficava violeta (`--accent-soft`), com relevo (`--bevel`) e quadrado (`--r-xs` num botão
 * de 44 px). Contrato:
 * 1. uma classe só para o item (`.nd-segment-item`), botão ou rótulo de rádio;
 * 2. raio aninhado (barra − padding), no desktop e no celular, e o item ocupa a faixa, centrado;
 * 3. o escolhido usa o pressionado neutro e o semibold, sem violeta nem relevo; abas, opções,
 *    cartões e árvore continuam com a regra comum de selecionado.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../test-utils";
import { RADII, SPACE } from "./tokens";

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");
const desktopCss = read("desktop/theme/desktop.css");
const shellCss = read("desktop/theme/shell.css");
const mobileCss = read("desktop/theme/mobile.css");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (/\.(tsx?|css)$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(full);
  }
  return out;
}

const sources = walk(SRC).map((full) => ({ file: relative(SRC, full).split(sep).join("/"), text: readFileSync(full, "utf8") }));
const SELECTED = ['.nd-root .nd-segment-item[aria-pressed="true"]', '.nd-root .nd-segment-item[data-checked="true"]'];
const token = (value: string, scale: Record<string, number>, prefix: string): number => {
  const name = new RegExp(`^var\\(--${prefix}-([\\w]+)\\)$`).exec(value.trim())?.[1];
  const px = name === undefined ? undefined : scale[name];
  if (px === undefined) throw new Error(`não é token --${prefix}: ${value}`);
  return px;
};
const prop = (body: string, name: string): string => {
  const match = new RegExp(`(?:^|;)\\s*${name}:\\s*([^;]+);`).exec(body);
  if (match === null) throw new Error(`${name} ausente`);
  return (match[1] ?? "").trim();
};
const radius = (body: string): number => token(prop(body, "border-radius"), RADII, "r");
const pad = (body: string): number => token(prop(body, "padding"), SPACE as unknown as Record<string, number>, "sp");

describe("controle segmentado: uma classe só", () => {
  it("a classe antiga do botão sumiu do código e do CSS", () => {
    const left = sources.filter((s) => s.text.includes("nd-segment-btn")).map((s) => s.file);
    expect(left).toEqual([]);
  });

  it("todo filho de um `.nd-segment` usa `.nd-segment-item`, com o texto num `<span>`", () => {
    const groups = sources.filter((s) => s.file.endsWith(".tsx") && /className="nd-segment[\s"]/.test(s.text));
    expect(groups.map((g) => g.file).sort()).toEqual(["apps/app/ConsolePanel.tsx", "apps/files/FilesApp.tsx", "apps/settings/AgentsForm.tsx"]);
    for (const { file, text } of groups) {
      const items = [...text.matchAll(/className="nd-segment-item"[\s\S]*?<\/(?:button|label)>/g)].map((m) => m[0]);
      expect({ file, items: items.length > 0 }).toEqual({ file, items: true });
      for (const item of items) expect({ file, span: /<span>[\s\S]*<\/span>/.test(item) }).toEqual({ file, span: true });
    }
  });
});

describe("controle segmentado: raio aninhado e item centrado na faixa", () => {
  it("desktop: raio do item = raio da barra − padding", () => {
    const bar = cssRuleBody(shellCss, ".nd-segment");
    expect(radius(cssRuleBody(shellCss, ".nd-segment-item"))).toBe(radius(bar) - pad(bar));
  });

  it("celular: a faixa e o item mudam juntos, com o item de 44 px", () => {
    const bar = cssRuleBody(mobileCss, '.nd-root[data-mobile="true"] .nd-segment');
    const item = cssRuleBody(mobileCss, '.nd-root[data-mobile="true"] .nd-segment-item');
    expect(radius(item)).toBe(radius(bar) - pad(cssRuleBody(shellCss, ".nd-segment")));
    expect(prop(item, "min-height")).toBe("44px");
  });

  it("o item ocupa a altura da faixa (sem alinhamento próprio na barra) e centra o texto", () => {
    const bar = cssRuleBody(shellCss, ".nd-segment");
    expect(bar).not.toMatch(/align-items/);
    const item = cssRuleBody(shellCss, ".nd-segment-item");
    expect(prop(item, "align-items")).toBe("center");
    expect(prop(item, "justify-content")).toBe("center");
    expect(prop(item, "border")).toBe("0");
    expect(cssRuleBody(shellCss, ".nd-segment-item > span")).toContain("text-overflow: ellipsis;");
  });
});

describe("controle segmentado: escolhido neutro no titânio", () => {
  const rules = cssRules(desktopCss).filter((r) => r.at === null);

  it("pressionado neutro e semibold, sem violeta nem relevo", () => {
    const rule = rules.find((r) => r.selectors.includes(SELECTED[0] ?? ""));
    expect(rule?.selectors).toEqual(SELECTED);
    expect(prop(rule?.body ?? "", "background-color")).toBe("var(--state-press)");
    expect(prop(rule?.body ?? "", "color")).toBe("var(--ink)");
    expect(prop(rule?.body ?? "", "font-weight")).toBe("var(--fw-semibold)");
  });

  it("nenhuma outra regra pinta o segmento escolhido (nem violeta, nem bisel)", () => {
    const allCss = sources.filter((s) => s.file.endsWith(".css"));
    const offenders = allCss.flatMap(({ file, text }) =>
      cssRules(text)
        .filter((r) => r.selectors.some((s) => s.includes("nd-segment-item") && /aria-pressed|data-checked/.test(s)))
        .filter((r) => /accent|bevel|box-shadow|background(?!-color: var\(--state-press\))/.test(r.body))
        .map((r) => `${file}: ${r.selectors.join(", ")}`),
    );
    expect(offenders).toEqual([]);
  });

  it("abas, opções, cartões e árvore continuam na regra comum de selecionado (--accent-soft)", () => {
    const common = rules.find((r) => r.body.includes("background-color: var(--accent-soft)"));
    for (const s of [
      '.nd-root .nd-tab[aria-selected="true"]',
      '.nd-root .nd-settings-tab[aria-selected="true"]',
      '.nd-root .nd-option[data-checked="true"]',
      '.nd-root .nd-radio-card[data-checked="true"]',
      '.nd-root .nd-board-item[data-selected="true"]',
      '.nd-root .nd-tree-item[data-selected="true"]',
      '.nd-root .nd-btn[aria-pressed="true"]',
    ]) {
      expect(common?.selectors).toContain(s);
    }
    expect((common?.selectors ?? []).filter((s) => s.includes("nd-segment"))).toEqual([]);
  });

  it("contraste alto: o escolhido ganha o contorno neutro, como a aba do celular", () => {
    const high = cssRules(desktopCss).find((r) => r.at?.includes("prefers-contrast: more") && r.selectors.some((s) => s.includes("nd-segment-item")));
    expect(high?.selectors).toContain('.nd-root .nd-mobile-tab[aria-current="page"]:not(:focus-visible)');
    expect(prop(high?.body ?? "", "outline")).toBe("1px solid var(--edge-strong)");
  });

  it("foco de teclado com o anel neutro, no botão e no rótulo de rádio", () => {
    const focus = cssRules(shellCss).find((r) => r.selectors.includes(".nd-root .nd-segment-item:focus-visible"));
    expect(focus?.selectors).toContain(".nd-root .nd-segment-item:has(> input:focus-visible)");
    expect(focus?.body).toContain("outline: 2px solid var(--focus-outer)");
  });
});
