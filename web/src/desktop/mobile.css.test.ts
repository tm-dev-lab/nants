/*
 * Regras de CSS do modo celular (spec 093, T10) e do botão principal desabilitado (pendência da
 * T09). O jsdom não calcula layout: aqui se conferem as regras; a captura real a 390 px é feita em navegador.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { FONT_SIZES, scaleVariables } from "./theme/tokens";

const read = (path: string): string => readFileSync(resolve(process.cwd(), path), "utf8");
const mobileCss = read("src/desktop/theme/mobile.css");
const shellCss = read("src/desktop/theme/shell.css");

/** Corpo da primeira regra com exatamente este seletor (sem os comentários). */
function ruleBody(css: string, selector: string): string {
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const index = clean.indexOf(`${selector} {`);
  if (index === -1) throw new Error(`regra ausente: ${selector}`);
  const start = clean.indexOf("{", index) + 1;
  return clean.slice(start, clean.indexOf("}", start));
}

/** Todos os .css sob `dir` (relativos a web/). */
function cssFiles(dir: string): string[] {
  return readdirSync(resolve(process.cwd(), dir), { recursive: true, encoding: "utf8" })
    .filter((name) => name.endsWith(".css"))
    .map((name) => join(dir, name));
}

type Specificity = [number, number, number];

/** Especificidade de um seletor simples (sem vírgula): `:where` vale 0; `:is` vale o argumento mais forte. */
function specificity(selector: string): Specificity {
  let rest = selector.replace(/:where\([^()]*\)/g, "");
  let extra: Specificity = [0, 0, 0];
  rest = rest.replace(/:is\(([^()]*)\)/g, (_, args: string) => {
    const best = args
      .split(",")
      .map((a) => specificity(a.trim()))
      .reduce((a, b) => (compare(a, b) >= 0 ? a : b), [0, 0, 0] as Specificity);
    extra = [extra[0] + best[0], extra[1] + best[1], extra[2] + best[2]];
    return " ";
  });
  rest = rest.replace(/\[[^\]]*\]/g, () => {
    extra = [extra[0], extra[1] + 1, extra[2]];
    return " ";
  });
  const ids = (rest.match(/#[\w-]+/g) ?? []).length;
  const classes = (rest.match(/\.[\w-]+/g) ?? []).length + (rest.match(/(?<!:):(?!:)[\w-]+/g) ?? []).length;
  const types = (rest.match(/(?:^|[\s>+~])[a-z][\w-]*/gi) ?? []).length + (rest.match(/::[\w-]+/g) ?? []).length;
  return [ids + extra[0], classes + extra[1], types + extra[2]];
}

function compare(a: Specificity, b: Specificity): number {
  return a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
}

describe("especificidade (auxiliar do teste)", () => {
  it("conta ids, classes, atributos, pseudo-classes e tipos; :is pelo mais forte; :where zero", () => {
    expect(specificity('.nd-root[data-mobile="true"] :is(input, textarea, select)')).toEqual([0, 2, 1]);
    expect(specificity(".nd-input.nd-mono")).toEqual([0, 2, 0]);
    expect(specificity(".nd-launcher-search input")).toEqual([0, 1, 1]);
    expect(specificity(".nd-root .nd-input:focus-visible")).toEqual([0, 3, 0]);
    expect(specificity(":where(.a, .b) input")).toEqual([0, 0, 1]);
  });
});

describe("modo celular: estrutura e alvos", () => {
  it("barra, área e abas empilhadas; a altura acompanha a viewport visual; sem rolagem horizontal", () => {
    const root = ruleBody(mobileCss, '.nd-root[data-mobile="true"]');
    expect(root).toContain("flex-direction: column;");
    expect(root).toContain("height: var(--app-height, 100%);");
    expect(root).toContain("overflow-x: hidden;");
    expect(ruleBody(mobileCss, '.nd-root[data-mobile="true"] .nd-workarea')).toContain("flex: 1;");
  });

  it("input, textarea e select com 16 px (sem zoom do iPhone ao focar), vencendo toda regra de fonte de campo", () => {
    const selector = '.nd-root[data-mobile="true"] :is(input, textarea, select)';
    // O tamanho vem da escala (094/T06): `--fs-touch` vale 16 px.
    expect(ruleBody(mobileCss, selector)).toContain("font-size: var(--fs-touch);");
    expect(FONT_SIZES.touch).toBe("16px");
    const ours = specificity(selector);
    const offenders: string[] = [];
    const seen: string[] = [];
    for (const file of cssFiles("src")) {
      if (file.endsWith("mobile.css")) continue;
      const clean = read(file).replace(/\/\*[\s\S]*?\*\//g, "");
      for (const match of clean.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        const [sel, body] = [match[1] ?? "", match[2] ?? ""];
        if (!/(?:^|;|\s)font(?:-size)?\s*:/.test(body)) continue;
        for (const part of sel.split(",").map((s) => s.trim())) {
          if (!/\b(?:input|textarea|select)\b|nd-input|nd-composer-input|nd-textarea/.test(part)) continue;
          seen.push(part);
          if (compare(specificity(part), ours) >= 0) offenders.push(`${file}: ${part}`);
        }
      }
    }
    // A varredura achou as regras de fonte dos campos (não passa por estar vazia).
    expect(seen).toEqual(expect.arrayContaining([".nd-input", ".nd-input.nd-mono", ".nd-composer-input", ".nd-affix input"]));
    expect(offenders).toEqual([]);
  });

  it("a barra respeita a área segura do topo e as abas a da base (notch e indicador do iPhone)", () => {
    expect(ruleBody(mobileCss, '.nd-root[data-mobile="true"] .nd-topbar')).toContain("env(safe-area-inset-top)");
    expect(ruleBody(mobileCss, ".nd-mobile-tabs")).toContain("env(safe-area-inset-bottom)");
  });

  it("alvos de toque com ≥ 44 px, inclusive os botões da barra de título (≥ 40 px)", () => {
    const targets = ruleBody(
      mobileCss,
      '.nd-root[data-mobile="true"] :where(button, select, summary, a.nd-btn, [role="tab"], [role="menuitem"], [role="menuitemradio"], [role="menuitemcheckbox"], [role="option"], [role="treeitem"])',
    );
    expect(targets).toContain("min-height: 44px;");
    const icons = ruleBody(mobileCss, '.nd-root[data-mobile="true"] :where(.nd-iconbtn, .nd-wbtn)');
    expect(icons).toContain("min-width: 44px;");
    expect(icons).toContain("min-height: 44px;");
    // 095/T02 (F-01): a busca compacta da barra é botão de ícone (44 × 44 px), como o sino.
    const topBar = read("src/desktop/topbar/TopBar.tsx");
    expect(topBar).toMatch(/compact \? "nd-btn nd-iconbtn nd-bar-btn nd-search-btn"/);
    expect(topBar).toMatch(/className="nd-btn nd-iconbtn nd-bell"/);
    expect(ruleBody(mobileCss, ".nd-mobile-tab")).toContain("min-height: 52px;");
    expect(ruleBody(mobileCss, ".nd-switcher-item")).toContain("min-height: 60px;");
  });

  it("abas alinhadas: a caixa do ícone tem o mesmo tamanho em todas, inclusive a do Alternador", () => {
    const icon = ruleBody(mobileCss, ".nd-mobile-tab-icon");
    expect(icon).toContain("width: 40px;");
    expect(icon).toContain("height: 28px;");
    expect(icon).toContain("flex-shrink: 0;");
    // O destaque do Alternador só pinta a caixa: não muda o tamanho (o rótulo não desce).
    const switcher = ruleBody(mobileCss, '.nd-mobile-tab[data-variant="switcher"] .nd-mobile-tab-icon');
    expect(switcher).not.toMatch(/\b(?:width|height|min-height|padding|margin)\s*:/);
    expect(ruleBody(mobileCss, ".nd-mobile-tab-label")).toContain("line-height: 14px;");
    // Nenhuma outra regra mexe no tamanho da caixa do ícone de uma aba só.
    const clean = mobileCss.replace(/\/\*[\s\S]*?\*\//g, "");
    const others = clean.match(/[^{}]*\.nd-mobile-tab-icon\s*\{[^}]*\}/g) ?? [];
    const sized = others.filter((rule) => /\b(?:width|height)\s*:/.test(rule.split("{")[1] ?? ""));
    expect(sized).toHaveLength(1);
  });

  it("o Alternador é uma lista vertical sobre superfície sólida (sem sobreposição)", () => {
    expect(ruleBody(mobileCss, ".nd-switcher-list")).toContain("flex-direction: column;");
    expect(ruleBody(mobileCss, ".nd-switcher")).toContain("background: var(--surface);");
    // Nenhum cartão empilhado por `transform` (como no mockup); `text-transform` não conta.
    expect(mobileCss).not.toMatch(/\.nd-switcher[^{]*\{[^}]*\stransform:/);
  });

  it("teclado aberto: a barra compacta e as abas somem só no modo celular", () => {
    expect(ruleBody(mobileCss, ':root[data-keyboard="open"] .nd-root[data-mobile="true"] .nd-keyboard-hide')).toContain(
      "display: none !important;",
    );
  });
});

describe("pílula no celular: Parar concêntrico, sem inflar (094/T05)", () => {
  const pill = '.nd-topbar-content[data-compact="true"] .nd-run-pill';
  const button = '.nd-topbar-content[data-compact="true"] .nd-pill-btn';
  /** Valor em px de uma propriedade: literal ou token da escala (094/T06), como `var(--sp-1)`. */
  const px = (body: string, name: string): number => {
    const match = new RegExp(`${name}:\\s*(?:(\\d+)px|var\\((--[\\w-]+)\\));`).exec(body);
    if (match === null) throw new Error(`${name} ausente`);
    if (match[1] !== undefined) return Number(match[1]);
    const token = scaleVariables()[(match[2] ?? "") as `--${string}`];
    if (token === undefined) throw new Error(`${name}: token fora da escala`);
    return Number.parseFloat(token);
  };

  it("raio interno = raio da pílula menos o padding; padding uniforme de 4 a 6 px", () => {
    const outer = ruleBody(mobileCss, pill);
    const radius = px(outer, "--pill-radius");
    const pad = px(outer, "--pill-pad");
    expect(pad).toBeGreaterThanOrEqual(4);
    expect(pad).toBeLessThanOrEqual(6);
    expect(outer).toContain("border-radius: var(--pill-radius);");
    // Padding uniforme em volta do botão (topo, direita e base); a esquerda é a do ponto e do texto.
    expect(outer).toContain("padding: var(--pill-pad) var(--pill-pad) var(--pill-pad) var(--sp-3);");
    const inner = ruleBody(mobileCss, button);
    expect(inner).toContain("border-radius: calc(var(--pill-radius) - var(--pill-pad));");
    // Nem pílula dentro de pílula: o raio interno é finito e menor que o externo.
    expect(radius - pad).toBeGreaterThan(0);
    expect(radius - pad).toBeLessThan(radius);
  });

  it("altura visível menor que a pílula: o botão ocupa a altura útil (pílula menos o padding), sem mínimo de 44 px", () => {
    const outer = ruleBody(mobileCss, pill);
    expect(outer).toContain("min-height: 44px;");
    const inner = ruleBody(mobileCss, button);
    expect(inner).toContain("align-self: stretch;");
    expect(inner).toContain("min-height: 0;");
    expect(inner).not.toMatch(/(?:^|;|\s)height:\s*\d/);
    // 44 px da pílula, menos a borda (no máximo 2) e o padding (2 × `--sp-1`): até 34 px visíveis.
    const visible = 44 - 2 - 2 * px(outer, "--pill-pad");
    expect(visible).toBeLessThan(44);
  });

  it("a regra geral de 44 px não infla o botão: a da pílula é mais específica", () => {
    const general =
      '.nd-root[data-mobile="true"] :where(button, select, summary, a.nd-btn, [role="tab"], [role="menuitem"], [role="menuitemradio"], [role="menuitemcheckbox"], [role="option"], [role="treeitem"])';
    expect(ruleBody(mobileCss, general)).toContain("min-height: 44px;");
    expect(compare(specificity(button), specificity(general))).toBeGreaterThan(0);
    const clean = mobileCss.replace(/\/\*[\s\S]*?\*\//g, "");
    // Nenhuma outra regra do celular dá altura mínima ao botão da pílula.
    const others = (clean.match(/[^{}]*\.nd-pill-btn[^{}]*\{[^}]*\}/g) ?? []).filter((rule) => /min-height:\s*[1-9]/.test(rule));
    expect(others).toEqual([]);
  });

  it("o alvo de toque de 44 px vem de uma área expandida invisível (pseudo-elemento)", () => {
    const area = ruleBody(mobileCss, `${button}::before`);
    expect(area).toContain('content: "";');
    expect(area).toContain("position: absolute;");
    expect(area).toContain("height: 44px;");
    expect(area).not.toMatch(/background|border|box-shadow/);
    expect(ruleBody(mobileCss, button)).toContain("position: relative;");
  });
});

describe("janela estreita (data-size): nada rola na horizontal a 390 px", () => {
  it("o conteúdo estreito corta a rolagem horizontal e quebra o que é longo", () => {
    expect(ruleBody(mobileCss, '.nd-content[data-size="narrow"]')).toContain("overflow-x: hidden;");
    expect(ruleBody(mobileCss, '.nd-content[data-size="narrow"] :where(img, video, canvas, table)')).toContain("max-width: 100%;");
    const pre = ruleBody(mobileCss, '.nd-content[data-size="narrow"] :where(pre)');
    expect(pre).toContain("white-space: pre-wrap;");
    expect(pre).toContain("overflow-wrap: anywhere;");
    expect(
      ruleBody(mobileCss, '.nd-content[data-size="narrow"] :where(p, li, dd, dt, td, th, h1, h2, h3, h4, label, summary, a, code)'),
    ).toContain("overflow-wrap: anywhere;");
    expect(ruleBody(mobileCss, '.nd-content[data-size="narrow"] :where(.nd-actions, .nd-dialog-actions)')).toContain("flex-wrap: wrap;");
  });

  it("o iframe do App não é limitado (os tamanhos fixos cabem por transform: scale)", () => {
    expect(mobileCss).not.toMatch(/data-size="narrow"\][^{]*iframe/);
  });
});

describe("botão principal desabilitado (pendência da T09)", () => {
  it("usa os tokens de controle desabilitado, sem opacidade por cima", () => {
    const body = ruleBody(shellCss, '.nd-btn[data-variant="primary"]:disabled');
    // 095/T02: o fundo neutro de controle do conjunto único (`--control`), sem o violeta.
    expect(body).toContain("background: var(--control);");
    expect(body).toContain("color: var(--muted);");
    expect(body).toContain("opacity: 1;");
  });
});

describe("095/T07 A-08: ferramentas da janela no celular numa linha compacta de 44 px", () => {
  const SLOT = '.nd-root[data-mobile="true"] .nd-window .nd-window-tools';

  it("altura fixa de 44 px, sem crescer na coluna da janela; rola na horizontal", () => {
    const body = ruleBody(mobileCss, SLOT);
    expect(body).toContain("flex: 0 0 44px;");
    expect(body).toContain("height: 44px;");
    expect(body).toContain("max-height: 44px;");
    expect(ruleBody(mobileCss, '.nd-root[data-mobile="true"] .nd-window-tools')).toContain("overflow-x: auto;");
  });

  it("vence toda regra de janela que mexe no flex ou na altura do slot (ex.: o App com flex: 1 1 auto)", () => {
    const mine = specificity(SLOT);
    const losers: string[] = [];
    for (const file of cssFiles("src")) {
      const clean = read(file).replace(/\/\*[\s\S]*?\*\//g, "");
      for (const m of clean.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        const body = m[2] ?? "";
        if (!/(?:^|;)\s*(?:flex|flex-grow|flex-basis|height|min-height)\s*:/.test(body)) continue;
        for (const raw of (m[1] ?? "").split(",")) {
          const selector = raw.trim();
          if (selector === SLOT || !/\.nd-window-tools(?![\w-])/.test(selector) || /:empty/.test(selector)) continue;
          if (compare(specificity(selector), mine) >= 0) losers.push(`${file}: ${selector}`);
        }
      }
    }
    expect(losers).toEqual([]);
    // O caso da captura existe e perde: o App estica o slot só na barra de título do desktop.
    expect(read("src/apps/app/app.css")).toContain('.nd-window[data-window="app"] .nd-window-tools');
  });
});
