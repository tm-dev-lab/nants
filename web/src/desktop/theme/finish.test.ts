// @vitest-environment node
/*
 * Escala de acabamento (spec 094, T06) e material titânio com reflexo (spec 095, T01). O jsdom não
 * aplica CSS: a escala é lida dos tokens e as regras, do CSS-fonte. A checagem de valores soltos
 * fica em `checks.test.ts`; aqui ficam a forma da escala, os raios concêntricos, a elevação por
 * tema, o material nos componentes, as janelas sem borda e os estados iguais em todos os controles.
 * O destino de cada checagem da 094 está na tabela "Migração das checagens" da 095/T01.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../test-utils";
import { contrastRatio, parseColor, titaniumExtremes } from "./contrast";
import {
  DISABLED_OPACITY,
  ELEVATION_LEVEL,
  ELEVATIONS,
  FLASH_RING,
  FONT_SIZES,
  FONT_WEIGHTS,
  RADII,
  scaleVariables,
  SELECTED_RING,
  SPACE,
  THEME_NAMES,
  THEMES,
  themeVariables,
} from "./tokens";

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");
const desktopCss = read("desktop/theme/desktop.css");
const shellCss = read("desktop/theme/shell.css");
const mobileCss = read("desktop/theme/mobile.css");
const boardCss = read("apps/board/board.css");

function walk(dir: string, ext: RegExp): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full, ext));
    else if (ext.test(name)) out.push(full);
  }
  return out;
}

const rel = (full: string): string => relative(SRC, full).split(sep).join("/");
const allCss = (): { file: string; css: string }[] =>
  ["desktop", "apps"].flatMap((d) => walk(join(SRC, d), /\.css$/)).map((full) => ({ file: rel(full), css: readFileSync(full, "utf8") }));

/** Classes que os componentes combinam com o titânio (barra, títulos, abas do celular e segmentados). */
function materialClassNames(): Set<string> {
  const out = new Set<string>();
  for (const full of walk(SRC, /\.tsx$/).filter((f) => !f.endsWith(".test.tsx"))) {
    for (const m of readFileSync(full, "utf8").matchAll(/className=(?:"[^"]*"|\{[^}]*\})/g)) {
      if (!/\bnd-chrome\b/.test(m[0])) continue;
      for (const cls of m[0].match(/nd-[\w-]+/g) ?? []) if (cls !== "nd-chrome") out.add(cls);
    }
  }
  return out;
}

/** Valor em px de um token de escala (`var(--r-lg)` → 12). */
function tokenPx(value: string): number {
  const vars = scaleVariables();
  const name = /^var\((--[\w-]+)\)$/.exec(value.trim())?.[1];
  const resolved = name === undefined ? undefined : vars[name as `--${string}`];
  if (resolved === undefined) throw new Error(`não é token da escala: ${value}`);
  return Number.parseFloat(resolved);
}

/** Valor de uma propriedade num corpo de regra. */
function prop(body: string, name: string): string {
  const match = new RegExp(`(?:^|;)\\s*${name}:\\s*([^;]+);`).exec(body);
  if (match === null) throw new Error(`${name} ausente`);
  return (match[1] ?? "").trim();
}

/** Maior desfoque (px) de uma lista de sombras. */
function maxBlur(shadow: string): number {
  return Math.max(...[...shadow.matchAll(/(?:^|,\s*)-?\d+(?:px)? -?\d+px (\d+)px/g)].map((m) => Number(m[1])));
}

/** Maior alfa entre as cores de uma lista de sombras. */
function maxAlpha(shadow: string): number {
  return Math.max(...[...shadow.matchAll(/rgba\([^)]*\)/g)].map((m) => parseColor(m[0]).a));
}

describe("escala de acabamento: forma", () => {
  it("raios xs < sm < md < lg < xl < 2xl < pill", () => {
    const order = [RADII.xs, RADII.sm, RADII.md, RADII.lg, RADII.xl, RADII["2xl"], RADII.pill];
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(new Set(order).size).toBe(order.length);
  });

  it("espaçamentos em múltiplos de 4: --sp-n vale n × 4", () => {
    for (const [step, px] of Object.entries(SPACE)) {
      expect(px % 4).toBe(0);
      expect(px).toBe(Number(step) * 4);
    }
  });

  it("tipografia por papel (095/T02, C-09): escala crescente, pesos sem o 700 (C-15)", () => {
    for (const role of ["micro", "caption", "label", "control", "body", "heading", "display", "numeral", "mono"] as const) expect(FONT_SIZES[role]).toMatch(/^\d+px$/);
    const order = (["micro", "caption", "label", "control", "body", "heading", "display", "numeral"] as const).map((r) => Number.parseFloat(FONT_SIZES[r]));
    expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(new Set(order).size).toBe(order.length);
    expect(FONT_SIZES.touch).toBe("16px");
    expect(Object.values(FONT_WEIGHTS)).toEqual([400, 500, 600]);
    // O título da janela usa o papel de controle (13) e o peso da escala.
    const title = cssRuleBody(desktopCss, ".nd-title");
    expect(title).toContain("font-size: var(--fs-control)");
    expect(title).toContain("font-weight: var(--fw-semibold)");
  });

  it("as variáveis da escala estão nos dois temas, com o mesmo valor", () => {
    const scale = scaleVariables();
    expect(scale["--r-lg"]).toBe("12px");
    expect(scale["--sp-3"]).toBe("12px");
    expect(scale["--fs-body"]).toBe("14px");
    expect(scale["--o-disabled"]).toBe(String(DISABLED_OPACITY));
    expect(scale["--ring-selected"]).toBe(SELECTED_RING);
    expect(scale["--ring-flash"]).toBe(FLASH_RING);
    for (const theme of THEME_NAMES) {
      const vars = themeVariables(theme);
      for (const [name, value] of Object.entries(scale)) expect(vars[name as `--${string}`]).toBe(value);
    }
  });
});

describe("raios aninhados concêntricos (interno = externo − padding)", () => {
  /** `pad`: onde está o padding entre o externo e os internos, quando não é o do próprio externo. */
  const cases: { label: string; outer: [string, string]; inner: [string, string][]; pad?: [string, string] }[] = [
    { label: "menu e itens", outer: [desktopCss, ".nd-menu"], inner: [[desktopCss, ".nd-menu-item"]] },
    {
      // 095/T02: o campo encosta no topo do painel (raio só em cima, igual ao do painel); os
      // resultados ficam na lista com padding de 8 px.
      label: "lançador e resultados",
      outer: [shellCss, ".nd-launcher"],
      pad: [shellCss, ".nd-launcher-list"],
      inner: [[shellCss, ".nd-launcher-item"]],
    },
    {
      label: "controle segmentado e segmentos",
      outer: [shellCss, ".nd-segment"],
      inner: [[shellCss, ".nd-segment-item"]],
    },
    {
      // 131/T07: no celular o item tem 44 px, e a faixa sobe para `--r-lg` (item `--r-md`).
      label: "controle segmentado no celular",
      outer: [mobileCss, '.nd-root[data-mobile="true"] .nd-segment'],
      pad: [shellCss, ".nd-segment"],
      inner: [[mobileCss, '.nd-root[data-mobile="true"] .nd-segment-item']],
    },
    { label: "lista do combo e opções", outer: [shellCss, ".nd-combo-list"], inner: [[shellCss, ".nd-combo-list li"]] },
    { label: "popover Colunas do Quadro e opções", outer: [boardCss, ".nd-board-popover"], inner: [[boardCss, ".nd-board-check"]] },
    {
      label: "aviso sob a barra e seus botões",
      outer: [shellCss, ".nd-bar-alert"],
      inner: [
        [desktopCss, ".nd-btn"],
        [desktopCss, ".nd-wbtn"],
      ],
    },
    // 095/T07 (decisão 9): a dica, o aviso e a pílula saíram do `--r-pill`; o botão encostado no
    // padding fica com o raio do contêiner menos o padding (`--r-lg` − `--sp-1` = `--r-md`).
    { label: "dica e o dispensar", outer: [desktopCss, ".nd-hint"], inner: [[desktopCss, ".nd-hint .nd-wbtn"]] },
    { label: "aviso flutuante e o Desfazer", outer: [shellCss, ".nd-toast"], inner: [[shellCss, ".nd-toast .nd-btn"]] },
    { label: "pílula de execução e o Parar", outer: [shellCss, ".nd-run-pill"], inner: [[desktopCss, ".nd-pill-btn"]] },
  ];

  /*
   * Superfícies de titânio revistas e fora da regra, com o motivo. Toda outra superfície com
   * raio maior que o padding lateral precisa estar em `cases` (o teste de cobertura confere).
   */
  const EXEMPT: ReadonlyMap<string, string> = new Map([
    [".nd-toast-more", "rótulo de texto (`--r-md`), sem controle dentro"],
  ]);

  /** Padding que encosta o filho na borda: o lateral (o segundo valor, ou o único). */
  const sidePad = (padding: string): number => {
    const parts = padding.split(/\s+(?![^(]*\))/);
    const side = parts.length === 1 ? parts[0] : parts[1];
    return side === "0" || side === undefined ? 0 : tokenPx(side);
  };

  it.each(cases.map((c) => [c.label, c] as const))("%s", (_label, c) => {
    const outer = cssRuleBody(c.outer[0], c.outer[1]);
    const radius = tokenPx(prop(outer, "border-radius"));
    const pad = sidePad(prop(c.pad === undefined ? outer : cssRuleBody(c.pad[0], c.pad[1]), "padding"));
    expect(pad).toBeGreaterThan(0);
    for (const [css, selector] of c.inner) {
      expect({ selector, radius: tokenPx(prop(cssRuleBody(css, selector), "border-radius")) }).toEqual({ selector, radius: radius - pad });
    }
  });

  it("cobertura: toda superfície de titânio com raio maior que o padding está num par concêntrico", () => {
    const covered = new Set([...cases.map((c) => c.outer[1]), ...EXEMPT.keys()]);
    const missing: string[] = [];
    for (const cls of materialClassNames()) {
      const selector = `.${cls}`;
      // As regras do seletor na ordem do arquivo: a última declaração de cada propriedade vence.
      const body = allCss()
        .flatMap(({ css }) => cssRules(css))
        .filter((r) => r.at === null && r.selectors.includes(selector))
        .map((r) => r.body)
        .join(";");
      const last = (name: string): string | undefined => [...body.matchAll(new RegExp(`(?:^|;)\\s*${name}:\\s*([^;]+);`, "g"))].at(-1)?.[1]?.trim();
      const radius = last("border-radius");
      if (radius === undefined) continue;
      const padding = last("padding") ?? "0";
      const pad = sidePad(padding);
      const pill = radius === "var(--r-pill)";
      if (!covered.has(selector) && (pill || pad < tokenPx(radius))) missing.push(selector);
    }
    expect(missing).toEqual([]);
  });

  it("pílula do celular: Parar com o raio da pílula menos o padding, pelos tokens", () => {
    const pill = cssRuleBody(mobileCss, '.nd-topbar-content[data-compact="true"] .nd-run-pill');
    expect(pill).toContain("--pill-radius: var(--r-lg);");
    expect(pill).toContain("--pill-pad: var(--sp-1);");
    expect(RADII.lg - SPACE[1]).toBe(RADII.md);
  });
});

describe("elevação por tema", () => {
  for (const theme of THEME_NAMES) {
    it(`${theme}: 4 níveis (elevada baixa, janela atrás, menu, janela em foco) com a sombra crescendo`, () => {
      const vars = themeVariables(theme);
      expect(ELEVATIONS.map((e) => ELEVATION_LEVEL[e])).toEqual([1, 2, 3, 4]);
      const blurs = ELEVATIONS.map((e) => maxBlur(THEMES[theme].elevation[e]));
      expect([...blurs].sort((a, b) => a - b)).toEqual(blurs);
      for (const e of ELEVATIONS) expect(vars[`--elev-${ELEVATION_LEVEL[e]}`]).toBe(THEMES[theme].elevation[e]);
    });
  }

  it("no claro a elevação é mais suave que no escuro em todos os níveis", () => {
    for (const e of ELEVATIONS) expect(maxAlpha(THEMES.light.elevation[e])).toBeLessThan(maxAlpha(THEMES.dark.elevation[e]));
  });

  it("janela atrás --elev-2, em foco --elev-4; menus e popovers --elev-3", () => {
    expect(cssRuleBody(desktopCss, ".nd-window")).toContain("box-shadow: var(--elev-2)");
    expect(cssRuleBody(desktopCss, '.nd-window[data-active="true"]')).toContain("box-shadow: var(--elev-4)");
    expect(cssRuleBody(desktopCss, ".nd-menu")).toContain("box-shadow: var(--bevel), var(--elev-3)");
  });
});

describe("material titânio com reflexo consistente", () => {
  it("o reflexo aparece nos dois temas: o ponto mais claro se distingue do topo da base", () => {
    for (const theme of THEME_NAMES) {
      const { lightest } = titaniumExtremes(theme);
      expect(contrastRatio(lightest, THEMES[theme].titanium.base.top)).toBeGreaterThan(1.1);
    }
  });

  const materialClasses = materialClassNames;

  it("barra superior, barras de título, abas do celular e segmentados usam o titânio; as sobreposições não", () => {
    const classes = materialClasses();
    for (const cls of ["nd-topbar", "nd-titlebar", "nd-segment", "nd-mobile-tabs"]) expect(classes).toContain(cls);
    // Decisão do Arquiteto na revisão 1 da T01: menus, popovers, avisos e dicas são sólidos (`.nd-overlay`).
    for (const cls of ["nd-menu", "nd-popover", "nd-tip", "nd-toast", "nd-bar-alert", "nd-hint", "nd-launcher"]) expect(classes).not.toContain(cls);
    // Todo controle segmentado é titânio.
    for (const full of walk(SRC, /\.tsx$/).filter((f) => !f.endsWith(".test.tsx"))) {
      for (const m of readFileSync(full, "utf8").matchAll(/className="nd-segment(?=[\s"])[^"]*"/g)) expect({ file: rel(full), cls: m[0] }).toEqual({ file: rel(full), cls: expect.stringContaining("nd-chrome") });
    }
  });

  it("titânio: base --chrome e bisel; nenhuma sombra própria apaga o bisel", () => {
    const chrome = cssRuleBody(desktopCss, ".nd-chrome");
    expect(chrome).toContain("background: var(--chrome);");
    expect(chrome).toContain("box-shadow: var(--bevel);");
    const offenders: string[] = [];
    const classes = materialClasses();
    for (const { file, css } of allCss()) {
      for (const rule of cssRules(css)) {
        if (rule.at !== null) continue;
        if (!rule.selectors.some((s) => classes.has(s.replace(/^\./, "")))) continue;
        const shadow = /(?:^|;)\s*box-shadow:\s*([^;]+);/.exec(rule.body)?.[1];
        if (shadow !== undefined && shadow.trim() !== "none" && !shadow.includes("var(--bevel)")) offenders.push(`${file}: ${rule.selectors.join(", ")}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("pílula de execução, controle segmentado e botões da janela: sem borda nem material próprio", () => {
    // 095/T02 (decisão 9): fundo neutro de controle; o tom da fase só no ponto e no rótulo.
    const pill = cssRuleBody(shellCss, ".nd-run-pill");
    expect(pill).toContain("background: var(--control);");
    expect(pill).not.toMatch(/border(?!-radius)|box-shadow/);
    for (const rule of cssRules(shellCss).filter((r) => r.selectors.some((s) => s.startsWith(".nd-run-pill[data-tone=")))) {
      expect(rule.body).not.toMatch(/border|background:/);
    }
    // O segmentado recebe o titânio pela classe `.nd-chrome`: a regra dele não pinta fundo nem borda.
    expect(cssRuleBody(shellCss, ".nd-segment")).not.toMatch(/border:|background|box-shadow/);
    // Botões da janela: só o ícone; o hover é a camada comum de estado (sem tecla de material).
    expect(cssRuleBody(desktopCss, ".nd-wbtn")).toContain("border: 0;");
    expect(cssRules(desktopCss).filter((r) => r.selectors.includes(".nd-root .nd-wbtn:hover:not(:focus-visible)"))).toEqual([]);
  });

  it("bordas do material e das superfícies flutuantes: no máximo 1 px, nunca a cor de destaque", () => {
    const frame = [...materialClasses(), "nd-window", "nd-run-pill", "nd-segment", "nd-dialog", "nd-combo-list", "nd-desktop-empty"];
    const offenders: string[] = [];
    for (const { file, css } of allCss()) {
      for (const rule of cssRules(css)) {
        if (rule.at !== null) continue;
        if (!rule.selectors.some((s) => frame.some((c) => new RegExp(`^\\.${c}(\\[[^\\]]*\\])*$`).test(s)))) continue;
        for (const m of rule.body.matchAll(/(?:^|;)\s*(border(?:-[a-z]+)*):\s*([^;]+);/g)) {
          const value = m[2] ?? "";
          const width = /(\d*\.?\d+)px/.exec(value);
          if ((width !== null && Number(width[1]) > 1) || /--accent|--selected-edge|--pill-[a-z]+-edge|--win-edge/.test(value)) {
            offenders.push(`${file}: ${rule.selectors.join(", ")} ${m[1]}: ${value}`);
          }
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe("estados iguais em todos os controles", () => {
  const CONTROLS = [
    ".nd-btn",
    ".nd-wbtn",
    ".nd-menu-item",
    ".nd-pill-btn",
    ".nd-segment-item",
    ".nd-tab",
    ".nd-settings-tab",
    ".nd-nanty-tab",
    ".nd-launcher-item",
    ".nd-folder-item",
    ".nd-tree-item",
    ".nd-option",
    ".nd-step",
    ".nd-choice",
    ".nd-mobile-tab",
    ".nd-switcher-item",
    ".nd-board-card",
    ".nd-radio-card",
  ];
  const rules = cssRules(desktopCss);
  const with_ = (selector: string): string | undefined => rules.find((r) => r.at === null && r.selectors.includes(selector))?.body;

  it.each(CONTROLS)("%s: hover, pressionado e (se aplicável) desabilitado pela camada comum", (control) => {
    expect(with_(`.nd-root ${control}:hover:not(:disabled)`)).toContain("linear-gradient(var(--state-hover), var(--state-hover))");
    expect(with_(`.nd-root ${control}:active:not(:disabled)`)).toContain("linear-gradient(var(--state-press), var(--state-press))");
  });

  it("selecionado: fundo --accent-soft e texto --ink; com borda, --selected-edge", () => {
    const selected = rules.find((r) => r.selectors.includes('.nd-root .nd-tab[aria-selected="true"]'));
    expect(selected?.body).toContain("background-color: var(--accent-soft)");
    expect(selected?.body).toContain("color: var(--ink)");
    for (const s of ['.nd-root .nd-launcher-item[data-active="true"]', '.nd-root .nd-btn[aria-pressed="true"]']) {
      expect(selected?.selectors).toContain(s);
    }
    const edged = rules.find((r) => r.selectors.includes('.nd-root .nd-option[data-checked="true"]') && r.body.includes("border-color"));
    expect(edged?.body).toContain("border-color: var(--selected-edge)");
  });

  it("todo controle com borda na regra base e com fundo de selecionado também ganha a borda --selected-edge", () => {
    const background = rules.find((r) => r.at === null && r.body.includes("background-color: var(--accent-soft)") && r.selectors.every((x) => x.startsWith(".nd-root ")));
    const edge = rules.find((r) => r.at === null && r.body.includes("border-color: var(--selected-edge)") && r.selectors.every((x) => x.startsWith(".nd-root ")));
    expect(background).toBeDefined();
    expect(edge).toBeDefined();
    /** Borda visível na regra base do controle (em qualquer CSS): não `0`, `none` nem transparente. */
    const bordered = (cls: string): boolean =>
      allCss()
        .flatMap(({ css }) => cssRules(css))
        .filter((r) => r.at === null && r.selectors.includes(cls))
        .some((r) => [...r.body.matchAll(/(?:^|;)\s*border:\s*([^;]+);/g)].some((m) => !/^(0|none)$|transparent/.test((m[1] ?? "").trim())));
    const missing: string[] = [];
    for (const selector of background?.selectors ?? []) {
      const target = selector.replace(/^\.nd-root /, "");
      const cls = /^\.[\w-]+(?: li)?/.exec(target)?.[0] ?? target;
      if (!bordered(cls)) continue;
      const covered = (edge?.selectors ?? []).some((e) => e.replace(/^\.nd-root /, "").startsWith(target));
      if (!covered) missing.push(selector);
    }
    expect(missing).toEqual([]);
    // Os botões-alternância (Diff, Quebra de linha, Ocultos, Só alterados) estão cobertos; o fantasma não tem borda.
    expect(edge?.selectors).toContain('.nd-root .nd-btn[aria-pressed="true"]:not([data-variant="ghost"])');
  });

  it("desabilitado: a mesma opacidade em todos", () => {
    const disabled = rules.find((r) => r.selectors.includes(".nd-root .nd-btn:disabled"));
    expect(disabled?.body).toContain("opacity: var(--o-disabled)");
    for (const s of [".nd-root .nd-pill-btn:disabled", ".nd-root .nd-segment-item:disabled", ".nd-root .nd-menu-item:disabled"]) {
      expect(disabled?.selectors).toContain(s);
    }
  });

  it("nenhum componente redefine o fundo do hover, do pressionado ou do selecionado por conta própria", () => {
    const offenders: string[] = [];
    const state = /:hover|:active|\[aria-pressed="true"\]|\[aria-selected="true"\]|\[data-checked="true"\]|\[aria-current=|\[data-active="true"\]|\[data-selected="true"\]|\[data-open="true"\]/;
    for (const { file, css } of allCss()) {
      for (const rule of cssRules(css)) {
        for (const selector of rule.selectors) {
          if (selector.startsWith(".nd-root ")) continue;
          if (!CONTROLS.some((c) => selector.startsWith(c)) || !state.test(selector)) continue;
          // Variante de perigo (fechar) troca só a cor de fundo, por cima da camada comum.
          if (selector === '.nd-wbtn[data-variant="close"]:hover') continue;
          if (/(?:^|;)\s*background(?:-image)?:/.test(rule.body)) offenders.push(`${file}: ${selector}`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});

describe("janelas e barras sem borda (095/T01, decisão 2)", () => {
  /** A moldura e as barras: em qualquer CSS, com ou sem `.nd-root` e estados, e em `@media`. */
  const FRAME = /^(?:\.nd-root(?:\[[^\]]*\])* )?\.nd-(?:window|topbar|titlebar|mobile-tabs|chrome)(?:\[[^\]]*\]|:[\w-]+(?:\([^)]*\))?)*$/;

  /**
   * Bordas declaradas numa regra. Só o reset `border: 0`/`none` (o de um `<button>`) não conta:
   * ele tira a borda nativa, não desenha uma.
   */
  const borders = (body: string): string[] =>
    [...body.matchAll(/(?:^|;)\s*(border(?:-(?!radius)[a-z]+)*)\s*:\s*([^;]+);/g)]
      .filter((m) => !(m[1] === "border" && /^(?:0|none)$/.test((m[2] ?? "").trim())))
      .map((m) => `${m[1]}: ${(m[2] ?? "").trim()}`);

  it("nenhuma regra da moldura ou das barras declara border (nem 0,5 px), em nenhum contexto", () => {
    const offenders: string[] = [];
    let seen = 0;
    for (const { file, css } of allCss()) {
      for (const rule of cssRules(css)) {
        if (!rule.selectors.some((s) => FRAME.test(s))) continue;
        seen += 1;
        for (const b of borders(rule.body)) offenders.push(`${file}: ${rule.at ?? ""} ${rule.selectors.join(", ")} ${b}`);
      }
    }
    expect(seen).toBeGreaterThan(5);
    expect(offenders).toEqual([]);
  });

  it("a checagem pega borda na moldura, inclusive no celular e no contraste alto; o reset do botão passa", () => {
    const sample =
      ".nd-window { border: 0.5px solid red; } @media (x) { .nd-root .nd-titlebar { border-bottom: 1px solid red; } } " +
      '.nd-root[data-mobile="true"] .nd-window { border-width: 0.5px; } .nd-topbar { border: 0; } .nd-titlebar { border-radius: 4px; }';
    const hits = cssRules(sample).filter((r) => r.selectors.some((s) => FRAME.test(s)) && borders(r.body).length > 0);
    expect(hits.map((r) => r.selectors[0])).toEqual([".nd-window", ".nd-root .nd-titlebar", '.nd-root[data-mobile="true"] .nd-window']);
  });

  it("a separação é a sombra de dois níveis (atrás --elev-2, em foco --elev-4); o --win-edge não existe", () => {
    expect(cssRuleBody(desktopCss, ".nd-window")).toContain("box-shadow: var(--elev-2)");
    expect(cssRuleBody(desktopCss, '.nd-window[data-active="true"]')).toContain("box-shadow: var(--elev-4)");
    for (const theme of THEME_NAMES) {
      expect(themeVariables(theme)["--win-edge"]).toBeUndefined();
      expect(themeVariables(theme)["--win-edge-active"]).toBeUndefined();
    }
    for (const { file, css } of allCss()) expect({ file, uses: /--win-edge/.test(css) }).toEqual({ file, uses: false });
  });

  it("o anel de foco do teclado continua e o contraste alto acrescenta um outline com --edge-strong", () => {
    expect(cssRuleBody(desktopCss, ".nd-root :focus-visible")).toContain("outline: 2px solid var(--focus-outer)");
    const more = cssRules(desktopCss).filter((r) => r.at?.includes("prefers-contrast: more") === true);
    const outlined = more.filter((r) => r.body.includes("outline: 1px solid var(--edge-strong)")).flatMap((r) => r.selectors);
    for (const s of [".nd-root .nd-chrome", ".nd-root .nd-window"]) expect(outlined).toContain(s);
  });
});
