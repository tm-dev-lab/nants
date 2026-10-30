import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { DEFAULT_APPEARANCE, parseAppearance, resolveTheme } from "./appearance";
import { codeMirrorTheme } from "./codemirror";
import { parseColor } from "./contrast";
import { attentionFavicon, attentionFaviconSvg } from "./favicon";
import {
  BEVEL,
  BRAND_DARK,
  FAVICON_TOKENS,
  THEME_NAMES,
  THEMES,
  TITANIUM_GEOMETRY,
  titaniumCss,
  themeCss,
  themeVariables,
  WALLPAPER_IDS,
  WALLPAPER_LABELS,
  WALLPAPERS,
  wallpaperStyle,
} from "./tokens";

const CSS = readFileSync(resolve(process.cwd(), "src/desktop/theme/desktop.css"), "utf8");

/** Bloco CSS de um seletor (o primeiro que aparece). */
function ruleOf(selector: string, css = CSS): string {
  const start = css.indexOf(`${selector} {`);
  if (start === -1) return "";
  return css.slice(start, css.indexOf("}", start) + 1);
}

/** Bloco de um `@media`/`@supports` inteiro (chaves balanceadas). */
function atBlock(prefix: string): string {
  const start = CSS.indexOf(prefix);
  if (start === -1) return "";
  let depth = 0;
  for (let i = CSS.indexOf("{", start); i < CSS.length; i += 1) {
    if (CSS[i] === "{") depth += 1;
    else if (CSS[i] === "}") {
      depth -= 1;
      if (depth === 0) return CSS.slice(start, i + 1);
    }
  }
  return "";
}

// --- Cascata mínima do desktop.css (só o que as regras do desktop usam) ---

interface CssRule {
  selectors: string[];
  body: string;
  /** Condição do `@media`/`@supports` que envolve a regra ("" fora de qualquer um). */
  media: string;
  order: number;
}

/** Regras do arquivo, com o `@media` que as envolve. */
function cssRules(css = CSS): CssRule[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const rules: CssRule[] = [];
  const stack: string[] = [];
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === "{") {
      const header = text.slice(start, i).trim();
      if (header.startsWith("@")) {
        stack.push(header);
        start = i + 1;
      } else {
        const end = text.indexOf("}", i);
        rules.push({
          selectors: header.split(",").map((s) => s.trim().replace(/\s+/g, " ")),
          body: text.slice(i + 1, end),
          media: stack.join(" "),
          order: rules.length,
        });
        i = end;
        start = end + 1;
      }
    } else if (ch === "}") {
      stack.pop();
      start = i + 1;
    }
  }
  return rules;
}

/** Especificidade (classes + atributos + pseudo-classes) de um seletor. */
function specificity(selector: string): number {
  return (selector.match(/\.[\w-]+|\[[^\]]*\]|:(?!:)[\w-]+/g) ?? []).length;
}

/** O seletor casa com um elemento com essas classes/atributos dentro de `.nd-root`? */
function matches(selector: string, classes: string[], attrs: string[], focused: boolean): boolean {
  if (/[>+~]/.test(selector)) return false;
  const parts = selector.split(" ");
  const last = parts.pop() ?? "";
  if (parts.some((p) => p !== ".nd-root")) return false;
  if (/^[a-z*]/i.test(last)) return false; // seletor de tipo: não são os nossos componentes
  const tokens = last.match(/\.[\w-]+|\[[^\]]*\]|:(?!:)[\w-]+/g) ?? [];
  if (tokens.join("") !== last) return false;
  return tokens.every((t) => {
    if (t.startsWith(".")) return classes.includes(t.slice(1));
    if (t.startsWith("[")) return attrs.includes(t.slice(1, -1));
    return t === ":focus-visible" && focused;
  });
}

/** Regra que vence para a propriedade (maior especificidade; empate, a última). */
function winner(classes: string[], focused: boolean, prop: string, attrs: string[] = [], media = ""): CssRule | undefined {
  const declares = (body: string): boolean =>
    body.split(";").some((d) => {
      const name = d.split(":")[0]?.trim() ?? "";
      return name === prop || (prop === "border" && name.startsWith("border"));
    });
  let best: { rule: CssRule; spec: number } | undefined;
  for (const rule of cssRules()) {
    if (rule.media !== "" && (media === "" || !rule.media.includes(media))) continue;
    if (!declares(rule.body)) continue;
    for (const selector of rule.selectors) {
      if (!matches(selector, classes, attrs, focused)) continue;
      const spec = specificity(selector);
      if (best === undefined || spec > best.spec || (spec === best.spec && rule.order > best.rule.order)) best = { rule, spec };
    }
  }
  return best?.rule;
}

describe("tokens: temas e variáveis CSS", () => {
  it("os dois temas geram o mesmo conjunto de variáveis, todas com valor", () => {
    const dark = themeVariables("dark");
    const light = themeVariables("light");
    expect(Object.keys(light).sort()).toEqual(Object.keys(dark).sort());
    for (const value of [...Object.values(dark), ...Object.values(light)]) expect(value).not.toBe("");
  });

  it("o titânio com reflexo está nas variáveis: --chrome (4 camadas), --chrome-muted, --hi e --lo", () => {
    for (const theme of THEME_NAMES) {
      const vars = themeVariables(theme);
      const ti = THEMES[theme].titanium;
      expect(vars["--chrome"]).toBe(titaniumCss(ti));
      // Faixa de luz, ponto de brilho, faixa diagonal e a base de 3 paradas, nesta ordem.
      const chrome = vars["--chrome"] ?? "";
      expect(chrome.match(/(?:linear|radial)-gradient\(/g)).toEqual(["linear-gradient(", "radial-gradient(", "linear-gradient(", "linear-gradient("]);
      expect(chrome).toContain(`radial-gradient(${TITANIUM_GEOMETRY.glint.size} at ${TITANIUM_GEOMETRY.glint.at}, ${ti.glint}`);
      expect(chrome).toContain(`linear-gradient(${TITANIUM_GEOMETRY.streak.angle}deg, `);
      expect(chrome.endsWith(`linear-gradient(180deg, ${ti.base.top} 0%, ${ti.base.mid} ${ti.base.midAt}, ${ti.base.bottom} 100%)`)).toBe(true);
      expect(vars["--chrome-muted"]).toBe(THEMES[theme].chromeMuted);
      // A borda de luz no topo e a linha da base do bisel saem do titânio.
      expect(vars["--hi"]).toBe(ti.edgeTop);
      expect(vars["--lo"]).toBe(ti.edgeBottom);
    }
    expect(BEVEL).toBe("inset 0 1px 0 var(--hi), inset 0 -1px 0 var(--lo)");
  });

  it("o vidro, o ruído, o brilho e o hairline da 094 saíram (095/T01, migração das checagens)", () => {
    for (const theme of THEME_NAMES) {
      const vars = themeVariables(theme);
      for (const name of ["--sheen", "--noise", "--hair", "--glass", "--glass-overlay", "--glass-solid", "--title-lift"] as const) {
        expect({ name, value: vars[name] }).toEqual({ name, value: undefined });
      }
    }
  });

  it("estrutura interna das janelas: --divider, --lane e --card-edge por tema (Main.dc.html)", () => {
    // Os alfas e a geometria do mockup (`divider`, `lane` e `cardEdge` do `renderVals`).
    const expected = {
      dark: { divider: 0.07, lane: 0.025, edge: /^inset 0 1px 0 \S.*, 0 1px 2px / },
      light: { divider: 0.08, lane: 0.025, edge: /^0 1px 3px / },
    } as const;
    for (const theme of THEME_NAMES) {
      const t = THEMES[theme];
      const vars = themeVariables(theme);
      expect(vars["--divider"]).toBe(t.divider);
      expect(vars["--lane"]).toBe(t.lane);
      expect(vars["--card-edge"]).toBe(t.cardEdge);
      expect(parseColor(t.divider).a).toBe(expected[theme].divider);
      expect(parseColor(t.lane).a).toBe(expected[theme].lane);
      expect(t.cardEdge).toMatch(expected[theme].edge);
      // Translúcidos e neutros: estruturam sem tingir nem contornar.
      for (const color of [t.divider, t.lane]) {
        const c = parseColor(color);
        expect(Math.max(c.r, c.g, c.b) - Math.min(c.r, c.g, c.b)).toBeLessThanOrEqual(10);
      }
      // Linha clara sobre o escuro e escura sobre o claro.
      expect(parseColor(t.divider).r > 128).toBe(theme === "dark");
    }
  });

  it("as superfícies continuam as da marca: grafite/azul-noite no escuro, branco no claro", () => {
    for (const theme of THEME_NAMES) {
      const surface = parseColor(THEMES[theme].surface2);
      if (theme === "dark") expect(surface.b).toBeGreaterThan(surface.r);
      else expect(Math.min(surface.r, surface.g, surface.b)).toBeGreaterThanOrEqual(240);
    }
    expect(themeCss("dark")).toContain("--chrome: linear-gradient");
  });

  it("no claro, os ícones e a ação principal usam as variantes escuras da marca", () => {
    expect(THEMES.light.icon.cyan).toBe(BRAND_DARK.cyan);
    expect(THEMES.light.icon.violet).toBe(BRAND_DARK.violet);
    expect(THEMES.light.icon.amber).toBe(BRAND_DARK.amber);
    expect(THEMES.light.pill.cyan.ink).toBe(BRAND_DARK.cyan);
  });
});

describe("tokens: papéis de parede", () => {
  it("Pontos, Nebulosa, Aurora e Liso, com versão por tema", () => {
    expect(WALLPAPER_IDS.map((id) => WALLPAPER_LABELS[id])).toEqual(["Pontos", "Nebulosa", "Aurora", "Liso"]);
    for (const id of WALLPAPER_IDS) {
      expect(WALLPAPERS[id].dark.base).not.toBe(WALLPAPERS[id].light.base);
      for (const theme of THEME_NAMES) {
        const style = wallpaperStyle(id, theme);
        expect(style.backgroundColor).toBe(WALLPAPERS[id][theme].base);
      }
    }
    expect(wallpaperStyle("dots", "dark").backgroundSize).toBe("28px 28px");
    expect(wallpaperStyle("aurora", "light").backgroundImage).toMatch(/^linear-gradient\(160deg, /);
    expect(wallpaperStyle("plain", "dark").backgroundImage).toBeUndefined();
  });
});

describe("aparência", () => {
  it("valida a seção appearance e volta ao padrão no que for inválido", () => {
    expect(parseAppearance(undefined)).toEqual(DEFAULT_APPEARANCE);
    expect(DEFAULT_APPEARANCE.wallpaper).toBe("dots");
    expect(parseAppearance({ theme: "light", wallpaper: "aurora" })).toEqual({ theme: "light", wallpaper: "aurora" });
    expect(parseAppearance({ theme: "sepia", wallpaper: "nebula" })).toEqual({ theme: "dark", wallpaper: "nebula" });
    expect(parseAppearance({ theme: "auto", wallpaper: 3 })).toEqual({ theme: "auto", wallpaper: "dots" });
  });

  it("Automático segue o sistema", () => {
    expect(resolveTheme("auto", true)).toBe("dark");
    expect(resolveTheme("auto", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
  });
});

describe("CodeMirror e favicon pelos tokens", () => {
  it("o tema do CodeMirror existe em claro e escuro", () => {
    expect(codeMirrorTheme("dark").length).toBeGreaterThan(0);
    expect(codeMirrorTheme("light").length).toBeGreaterThan(0);
  });

  it("o favicon de atenção sai dos tokens", () => {
    const svg = attentionFaviconSvg();
    expect(svg).toContain(`fill="${FAVICON_TOKENS.dot}"`);
    expect(svg).toContain(`fill="${FAVICON_TOKENS.brand.cyan}"`);
    expect(attentionFavicon()).toBe(`data:image/svg+xml,${encodeURIComponent(svg)}`);
  });
});

describe("desktop.css: material, fallback e preferências do sistema", () => {
  it("o titânio usa as variáveis do material: --chrome com o bisel, opaco e sem desfoque", () => {
    const chrome = ruleOf(".nd-chrome");
    expect(chrome).toContain("background: var(--chrome);");
    // O bisel (borda de luz no topo + linha da base) vem do token `--bevel`.
    expect(chrome).toContain("box-shadow: var(--bevel);");
    // O conteúdo sobre o titânio usa as cores do cromo (C-01): texto, secundário, ícones, seleção e pílulas.
    for (const decl of [
      "--ink: var(--chrome-ink);",
      "--ink2: var(--chrome-ink);",
      "--ink3: var(--chrome-ink);",
      "--muted: var(--chrome-muted);",
      "--control-edge: var(--chrome-muted);",
      "--selected-edge: var(--chrome-ink);",
      "--danger-ink: var(--chrome-pill-rose-ink);",
    ]) {
      expect(chrome).toContain(decl);
    }
    for (const tone of ["violet", "cyan", "amber", "green", "rose", "muted"]) {
      expect(chrome).toContain(`--icon-${tone}: var(--chrome-icon-${tone});`);
      expect(chrome).toContain(`--pill-${tone}-ink: var(--chrome-pill-${tone}-ink);`);
    }
    expect(chrome).not.toMatch(/backdrop-filter|border/);
    // Sobreposições: superfície sólida, sem titânio nem variantes próprias de secundário e estados.
    const overlay = ruleOf(".nd-overlay");
    expect(overlay).toContain("background: var(--overlay);");
    expect(overlay).not.toMatch(/--chrome|--muted|--state-/);
    for (const theme of THEME_NAMES) {
      const vars = themeVariables(theme);
      expect(vars["--overlay"]).toBe(THEMES[theme].overlay);
      expect(vars["--overlay-edge"]).toBe(THEMES[theme].overlayEdge);
      for (const gone of ["--chrome-overlay", "--chrome-overlay-muted", "--chrome-overlay-hover", "--chrome-overlay-press"] as const) {
        expect({ gone, value: vars[gone] }).toEqual({ gone, value: undefined });
      }
    }
    // Sem vidro: nenhum desfoque nem fallback de vidro no desktop (o véu do menu do N, spec 137, é a
    // exceção única: desfoca de leve a área de trabalho atrás dele, não é superfície).
    const css = CSS.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\.nd-nmenu-scrim\s*\{[^}]*\}/g, "");
    expect(css).not.toMatch(/backdrop-filter|@supports not|prefers-reduced-transparency/);
  });

  it("contraste alto: o titânio vira sólido, com contorno --edge-strong (outline, nunca border)", () => {
    const more = atBlock("@media (prefers-contrast: more)");
    expect(more).toContain("background: var(--surface2)");
    expect(more).toContain("outline: 1px solid var(--edge-strong)");
    expect(more).not.toMatch(/(?:^|[;{\s])border(?:-[a-z]+)*:/);
  });

  it("foco duplo visível: contorno externo e anel interno", () => {
    const focus = ruleOf(".nd-root :focus-visible");
    expect(focus).toContain("outline: 2px solid var(--focus-outer)");
    expect(focus).toContain("box-shadow: 0 0 0 2px var(--focus-inner)");
  });

  it("barra de título: anel neutro interno (não é cortado pelo overflow da janela; 095/T07, decisão 2)", () => {
    const rules = cssRules().filter((r) => r.selectors.includes(".nd-root .nd-titlebar:focus-visible"));
    expect(rules).toHaveLength(1);
    const body = rules[0]?.body ?? "";
    expect(body).toContain("outline: 2px solid var(--focus-outer)");
    expect(body).toMatch(/outline-offset: -[2-9]px/);
    // Discreto: só o anel neutro, sem o anel interno; o bisel do titânio fica.
    expect(body).toContain("box-shadow: var(--bevel)");
    expect(ruleOf(".nd-window")).toContain("overflow: hidden");
    // A regra da barra vence a do material (mesma especificidade, depois no arquivo).
    expect(winner(["nd-titlebar", "nd-chrome"], true, "box-shadow")?.body).toBe(body);
  });

  it("nenhuma sombra de componente apaga o anel interno do foco", () => {
    // Elementos focáveis e as classes/atributos que eles podem ter ao mesmo tempo.
    const focusables: { label: string; classes: string[]; attrs?: string[] }[] = [
      { label: "app em foco na grade do menu do N", classes: ["nd-nmenu-tile"], attrs: ['data-state="focused"'] },
      { label: "opção marcada do Personalizar", classes: ["nd-choice"], attrs: ['aria-pressed="true"'] },
      { label: "barra de título", classes: ["nd-titlebar", "nd-chrome"] },
      { label: "folha de atalhos (titânio focável)", classes: ["nd-sheet", "nd-popover", "nd-chrome"] },
      { label: "botão", classes: ["nd-btn"], attrs: ['data-variant="primary"'] },
      { label: "botão da janela", classes: ["nd-wbtn"], attrs: ['data-variant="close"'] },
      { label: "item de menu", classes: ["nd-menu-item"] },
    ];
    for (const f of focusables) {
      // A barra de título usa só o anel neutro (095/T07, decisão 2): o bisel fica no lugar do anel interno.
      if (f.label === "barra de título") {
        expect(winner(f.classes, true, "box-shadow", f.attrs ?? [])?.body).toContain("box-shadow: var(--bevel)");
        expect(winner(f.classes, true, "outline", f.attrs ?? [])?.body).toContain("var(--focus-outer)");
        continue;
      }
      const rule = winner(f.classes, true, "box-shadow", f.attrs ?? []);
      expect({ label: f.label, shadow: rule?.body.includes("var(--focus-inner)") }).toEqual({ label: f.label, shadow: true });
      const outline = winner(f.classes, true, "outline", f.attrs ?? []);
      expect({ label: f.label, outline: outline?.body.includes("var(--focus-outer)") }).toEqual({ label: f.label, outline: true });
    }
  });

  it("contraste alto: o contorno --edge-strong vence na barra de título, na barra superior e na janela", () => {
    const forClasses = (classes: string[]): CssRule | undefined => winner(classes, false, "outline", [], "prefers-contrast: more");
    for (const classes of [["nd-titlebar", "nd-chrome"], ["nd-topbar", "nd-chrome"], ["nd-window"]]) {
      expect({ classes, outline: forClasses(classes)?.body.includes("outline: 1px solid var(--edge-strong)") }).toEqual({ classes, outline: true });
    }
  });

  it("iframes sem ponteiro durante o gesto e movimento reduzido sem transições", () => {
    expect(CSS).toContain('.nd-root[data-gesture="true"] iframe');
    expect(atBlock("@media (prefers-reduced-motion: reduce)")).toContain("transition: none");
  });
});
