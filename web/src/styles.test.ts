/*
 * Base da página: o jsdom não mede layout, então aqui se conferem as regras que dão altura ao
 * desktop (`html`, `body` e `#root` com 100%), as barras de rolagem finas pelos tokens e as metas
 * do navegador que acompanham o tema.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { applyThemeMeta } from "./desktop/theme/appearance";
import { THEMES } from "./desktop/theme/tokens";

const read = (path: string): string =>
  readFileSync(resolve(process.cwd(), path), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

/** Declarações de todas as regras cujo seletor (lista) inclui `selector`. */
function declarationsFor(css: string, selector: string): string {
  let out = "";
  for (const match of css.matchAll(/([^{}]+)\{([^}]*)\}/g)) {
    const selectors = (match[1] ?? "").split(",").map((s) => s.trim());
    if (selectors.includes(selector)) out += match[2] ?? "";
  }
  return out;
}

describe("altura da página", () => {
  const css = read("src/styles.css");

  it.each(["html", "body", "#root"])("%s ocupa a viewport (height: 100%%, margin: 0)", (selector) => {
    const body = declarationsFor(css, selector);
    expect(body).toMatch(/(^|;|\s)height:\s*100%/);
    expect(body).toMatch(/margin:\s*0/);
  });

  it("o .nd-root herda a altura (100%; no celular, --app-height com 100% de reserva)", () => {
    expect(declarationsFor(read("src/desktop/theme/desktop.css"), ".nd-root")).toMatch(/height:\s*100%/);
    expect(declarationsFor(read("src/desktop/theme/mobile.css"), '.nd-root[data-mobile="true"]')).toContain("height: var(--app-height, 100%)");
  });
});

describe("barras de rolagem", () => {
  it("finas nas janelas, com a cor da linha dos tokens", () => {
    const body = declarationsFor(read("src/desktop/theme/desktop.css"), ".nd-root *");
    expect(body).toContain("scrollbar-width: thin");
    expect(body).toContain("scrollbar-color: var(--line2) transparent");
  });
});

describe("index.html: ícones da marca", () => {
  it("referencia favicon (svg, ico, png), ícone do iPhone e manifest, sem favicon data:", () => {
    const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");
    expect(html).toContain('href="/brand/favicon.svg"');
    expect(html).toContain('href="/brand/favicon.ico"');
    expect(html).toContain('rel="apple-touch-icon"');
    expect(html).toContain('rel="manifest"');
    expect(html).toMatch(/<meta name="theme-color"/);
    expect(html).not.toContain("data:");
  });
});

describe("metas do navegador pelo tema", () => {
  afterEach(() => {
    for (const name of ["theme-color", "color-scheme"]) document.head.querySelector(`meta[name="${name}"]`)?.remove();
  });

  it("theme-color com o fundo dos tokens e color-scheme do tema; atualiza sem duplicar", () => {
    applyThemeMeta("light");
    applyThemeMeta("dark");
    applyThemeMeta("light");
    expect(document.head.querySelectorAll('meta[name="theme-color"]')).toHaveLength(1);
    expect(document.head.querySelector<HTMLMetaElement>('meta[name="theme-color"]')?.content).toBe(THEMES.light.bg);
    expect(document.head.querySelector<HTMLMetaElement>('meta[name="color-scheme"]')?.content).toBe("light");
  });
});
