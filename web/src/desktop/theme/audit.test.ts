// @vitest-environment node
/*
 * Achados da auditoria visual do Arquiteto (spec 094, T08, A-01 a A-15) travados no CSS-fonte e
 * nos tokens. O jsdom não aplica CSS: as regras são lidas dos arquivos. O comportamento de cada
 * janela (marcação, rótulos, estados vazios) fica nos testes de janela de cada app.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../test-utils";
import { parseColor } from "./contrast";
import { DANGER_INK, SCRIM_MIN_ALPHA, THEME_NAMES, THEMES, themeVariables } from "./tokens";

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");
const desktopCss = read("desktop/theme/desktop.css");
const shellCss = read("desktop/theme/shell.css");
const mobileCss = read("desktop/theme/mobile.css");
const appsCss = read("apps/shared/apps.css");
const boardCss = read("apps/board/board.css");
const activityCss = read("apps/activity/activity.css");
const costsCss = read("apps/costs/costs.css");
const filesCss = read("apps/files/files.css");
const appCss = read("apps/app/app.css");

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
const sources = (): { file: string; text: string }[] =>
  walk(SRC, /\.tsx$/)
    .filter((f) => !f.endsWith(".test.tsx"))
    .map((full) => ({ file: rel(full), text: readFileSync(full, "utf8") }));

/** Regra (em qualquer contexto, inclusive `@media`/`@supports`) cujo seletor inclui `selector`. */
function rulesWith(css: string, selector: string, at?: RegExp): { body: string; at: string | null }[] {
  return cssRules(css).filter((r) => r.selectors.includes(selector) && (at === undefined ? true : r.at !== null && at.test(r.at)));
}

/*
 * A-01 (adaptado na 095/T01): o vidro saiu. As sobreposições continuam opacas: são titânio, cuja
 * base é sólida (`titaniumExtremes` mede alfa 1 nos dois extremos), sem `backdrop-filter`.
 */
describe("A-01: sobreposições opacas (titânio), sem vidro nem desfoque", () => {
  it.each(THEME_NAMES)("%s: a base do titânio é opaca nas 3 paradas", (theme) => {
    const base = THEMES[theme].titanium.base;
    for (const stop of [base.top, base.mid, base.bottom]) expect(parseColor(stop).a).toBe(1);
  });

  it("nenhum CSS da interface usa backdrop-filter nem as classes de vidro", () => {
    const css = [desktopCss, shellCss, mobileCss, appsCss, boardCss, activityCss, costsCss, filesCss, appCss].map((c) => c.replace(/\/\*[\s\S]*?\*\//g, "")).join("\n");
    expect(css).not.toMatch(/backdrop-filter|\.nd-glass|\.nd-metal|overlay-glass|modal-glass/);
    for (const { file, text } of sources()) expect({ file, glass: /\bnd-(?:glass|metal|overlay-glass|modal-glass)\b/.test(text) }).toEqual({ file, glass: false });
  });

  /*
   * Decisão do Arquiteto na revisão 1 da T01: as sobreposições NÃO são titânio. Elas são a
   * superfície sólida `--overlay` (como a busca e o aviso do mockup); o titânio fica só nas barras,
   * nos títulos, no dock, nas abas do celular e nos segmentados.
   */
  it("toda superfície que flutua (menu, popover, sino, lançador, avisos, dicas) é .nd-overlay sólida, sem titânio", () => {
    const overlay = /\bnd-overlay(?![\w-])/;
    // 095/T02 (C-07): no celular as ferramentas da janela são uma linha de titânio sob a barra de título.
    const bars = /\bnd-(?:topbar|titlebar|dock|mobile-tabs|segment|window-tools)(?![\w-])/;
    const seen = new Set<string>();
    const wrong: string[] = [];
    for (const { file, text } of sources()) {
      for (const m of text.matchAll(/className=(?:"[^"]*"|\{[^}]*\})/g)) {
        const chrome = /\bnd-chrome\b/.test(m[0]);
        if (overlay.test(m[0])) {
          for (const cls of m[0].match(/nd-[\w-]+/g) ?? []) seen.add(cls);
          if (chrome) wrong.push(`sobreposição com titânio: ${file}: ${m[0]}`);
        }
        if (chrome && !bars.test(m[0])) wrong.push(`titânio fora de barra/título/dock/abas/segmentado: ${file}: ${m[0]}`);
      }
    }
    expect(wrong).toEqual([]);
    for (const cls of ["nd-menu", "nd-popover", "nd-sheet", "nd-attention", "nd-launcher", "nd-toast", "nd-toast-more", "nd-permission-toast", "nd-bar-alert", "nd-bar-note", "nd-hint", "nd-tip", "nd-board-popover"]) {
      expect(seen).toContain(cls);
    }
  });

  it.each(THEME_NAMES)("%s: a sobreposição é sólida, com a borda de luz no topo do mockup", (theme) => {
    expect(parseColor(THEMES[theme].overlay).a).toBe(1);
    expect(THEMES[theme].overlayEdge).toMatch(/^inset 0 1px 0 rgba\(/);
    const body = cssRuleBody(desktopCss, ".nd-overlay");
    expect(body).toContain("background: var(--overlay);");
    expect(body).toContain("--bevel: var(--overlay-edge);");
    expect(body).not.toMatch(/--chrome|border:/);
  });

  it("diálogos: superfície sólida (sem vidro)", () => {
    expect(cssRuleBody(shellCss, ".nd-dialog")).toContain("background: var(--surface);");
  });
});

describe("V-01: sobreposições modais (lançador, diálogos) com painel sólido e véu escuro", () => {
  it.each(THEME_NAMES)("%s: --scrim com alfa ≥ o mínimo do tema; o painel modal é opaco", (theme) => {
    expect(parseColor(THEMES[theme].scrim).a).toBeGreaterThanOrEqual(SCRIM_MIN_ALPHA[theme]);
    expect(themeVariables(theme)["--scrim"]).toBe(THEMES[theme].scrim);
    expect(parseColor(THEMES[theme].surface2).a).toBe(1);
  });

  it("o véu do lançador e dos diálogos é o --scrim", () => {
    for (const selector of [".nd-launcher-backdrop", ".nd-dialog-backdrop"]) {
      expect({ selector, body: cssRuleBody(shellCss, selector) }).toEqual({ selector, body: expect.stringContaining("background: var(--scrim);") });
    }
  });

  it("o painel modal (lançador): a sobreposição sólida, sem borda (C-16) e sem desfoque", () => {
    const body = cssRuleBody(shellCss, ".nd-launcher");
    expect(body).not.toMatch(/border:|backdrop-filter|background/);
    expect(body).toContain("box-shadow: var(--bevel), var(--elev-4);");
  });

  it("o lançador é o painel sólido; nenhum filho dele tem fundo translúcido próprio", () => {
    const launcher = sources().find((s) => s.file === "desktop/Launcher.tsx")?.text ?? "";
    expect(launcher).toMatch(/className="nd-launcher nd-overlay"/);
    const offenders = cssRules(shellCss)
      .filter((r) => r.selectors.some((s) => /^\.nd-launcher-(?!backdrop)/.test(s)))
      .filter((r) => /(?:^|;)\s*background(?:-color)?:\s*[^;]*(?:rgba|--glass(?!-solid)|--accent-soft|--shadow|--scrim)/.test(r.body))
      .map((r) => r.selectors.join(", "));
    expect(offenders).toEqual([]);
  });

  it("diálogos: painel sólido (--surface) sobre o véu", () => {
    expect(cssRuleBody(shellCss, ".nd-dialog")).toContain("background: var(--surface);");
  });
});

describe("F-01: a troca de coluna anima o cartão inteiro (o item, que é a moldura)", () => {
  it("nd-board-enter mira .nd-board-item, e o movimento reduzido também", () => {
    const anim = cssRules(boardCss).filter((r) => /nd-board-enter/.test(r.body));
    expect(anim.map((r) => r.selectors)).toEqual([['.nd-board-item[data-enter="true"]']]);
    const reduced = rulesWith(boardCss, '.nd-board-item[data-enter="true"]', /prefers-reduced-motion/);
    expect(reduced[0]?.body).toContain("animation: none;");
    expect(boardCss).not.toMatch(/\[data-enter="true"\] \.nd-board-card/);
  });
});

describe("A-02 e A-09: linha de metadados única, sem serifa, rótulo (12 px, escala da 095) e --muted", () => {
  it("a regra comum", () => {
    const body = cssRuleBody(appsCss, ".nd-meta-line");
    expect(body).toContain("font-family: var(--font-sans);");
    expect(body).toContain("font-size: var(--fs-label);");
    expect(body).toContain("color: var(--muted);");
    expect(body).toContain("flex-wrap: wrap;");
  });

  it("Quadro: o item é a moldura do cartão (o rodapé com as ações fica dentro); a seleção marca o item", () => {
    // 095/T05: relevo `--card-edge` sem contorno; a borda transparente só serve à seleção.
    const item = cssRuleBody(boardCss, ".nd-board-item");
    expect(item).toContain("border: 1px solid transparent;");
    expect(item).toContain("box-shadow: var(--card-edge);");
    expect(item).toContain("background: var(--card);");
    const card = cssRuleBody(boardCss, ".nd-board-card");
    expect(card).toContain("border: 0;");
    expect(card).toContain("background: transparent;");
    expect(cssRuleBody(boardCss, ".nd-board-card-foot")).toContain("display: flex;");
    const selected = cssRules(desktopCss).filter((r) => r.selectors.includes('.nd-root .nd-board-item[data-selected="true"]'));
    expect(selected.map((r) => r.body).join(";")).toMatch(/background-color: var\(--accent-soft\)[\s\S]*border-color: var\(--selected-edge\)/);
  });

  it("Plano: sem a caixa de numeração das tarefas", () => {
    expect(appsCss).not.toContain(".nd-plan-task-index");
    for (const { text } of sources()) expect(text).not.toContain("nd-plan-task-index");
  });
});

describe("A-03: coluna vazia do Quadro sem a caixa tracejada", () => {
  it("rótulo discreto centralizado", () => {
    expect(boardCss).not.toContain(".nd-board-placeholder");
    expect(boardCss).not.toMatch(/dashed var\(--line2\)/);
    const body = cssRuleBody(boardCss, ".nd-board-empty");
    expect(body).toContain("text-align: center;");
    expect(body).toContain("color: var(--muted);");
  });
});

describe("A-04: selo curto numa linha", () => {
  it(".nd-pill-nowrap não quebra nem encolhe", () => {
    const body = cssRuleBody(appsCss, ".nd-pill-nowrap");
    expect(body).toContain("white-space: nowrap;");
    expect(body).toContain("flex-shrink: 0;");
  });
});

describe("A-05: texto do agente parado em sem serifa", () => {
  it(".nd-agent-card-idle", () => {
    const body = cssRuleBody(activityCss, ".nd-agent-card-idle");
    expect(body).toContain("font-family: var(--font-sans);");
    expect(body).toContain("color: var(--muted);");
  });
});

describe("A-06: Custos com respiro entre as seções e margem inferior na rolagem", () => {
  it("gap entre as seções (o do mockup na 095/T05) e o espaçador final de --sp-4", () => {
    expect(cssRuleBody(costsCss, ".nd-costs")).toContain("gap: var(--sp-4);");
    expect(cssRuleBody(costsCss, ".nd-costs::after")).toContain("margin-top: calc(var(--sp-4) * -1);");
    const after = cssRuleBody(costsCss, ".nd-costs::after");
    expect(after).toContain('content: "";');
    expect(after).toContain("height: var(--sp-4);");
    expect(after).toContain("flex: none;");
  });
});

describe("A-07: Arquivos com orientação no lugar do visualizador", () => {
  it("o lado vazio tem a hairline de separação", () => {
    expect(cssRuleBody(filesCss, ".nd-files-placeholder")).toContain("border-left: 1px solid var(--divider);");
  });
});

describe("A-08 e A-10: blocos recolhíveis mostram que abrem", () => {
  it("o chevron gira quando o bloco abre", () => {
    expect(cssRuleBody(appsCss, ".nd-details[open] > summary .nd-details-chevron")).toContain("transform: rotate(90deg);");
  });

  it("todo <summary> de um .nd-details tem o chevron (o display flex apaga o marcador nativo)", () => {
    const missing: string[] = [];
    for (const { file, text } of sources()) {
      for (const m of text.matchAll(/<details[^>]*className="nd-details[^"]*"[^>]*>\s*<summary[\s\S]*?<\/summary>/g)) {
        if (!m[0].includes("nd-details-chevron")) missing.push(`${file}: ${m[0].slice(0, 80)}`);
      }
    }
    expect(missing).toEqual([]);
  });
});

describe("A-11: Ir para o fim e composer do celular na escala", () => {
  it("botão com elevação, no canto inferior direito (raio moderado `--r-lg`, 095 decisão 9)", () => {
    const body = cssRuleBody(appsCss, ".nd-jump");
    expect(body).toContain("border-radius: var(--r-lg);");
    expect(body).toContain("right: var(--sp-3);");
    expect(body).toContain("bottom: var(--sp-3);");
    expect(body).toContain("box-shadow: var(--bevel), var(--elev-3);");
  });

  it("composer do celular: sem gap e com o padding mínimo", () => {
    const body = cssRuleBody(mobileCss, '.nd-root[data-mobile="true"] .nd-composer');
    expect(body).toContain("gap: 0;");
    expect(body).toContain("padding: var(--sp-1) var(--sp-1) var(--sp-1) var(--sp-1);");
  });
});

describe("A-12 e 095/T07 A-09: Alternador em linhas neutras; só a vista atual selecionada", () => {
  it("linha com --divider (sem contorno nem cartão) e o atual por aria-current, só com fundo --lane", () => {
    const item = cssRuleBody(mobileCss, ".nd-switcher-item");
    expect(item).toContain("border: 0;");
    expect(item).toContain("border-bottom: 1px solid var(--divider);");
    expect(item).toContain("background: transparent;");
    expect(cssRuleBody(mobileCss, ".nd-switcher-list")).toContain("gap: 0;");
    // Ícone neutro como no dock: nem tom por aplicativo no componente.
    expect(cssRuleBody(mobileCss, ".nd-switcher-icon")).toContain("color: var(--ink2);");
    // Fora do contraste alto (onde o atual ganha o contorno neutro --edge-strong, 095/T09).
    const current = cssRules(desktopCss).filter(
      (r) => (r.at ?? null) === null && r.selectors.some((s) => s.includes('.nd-switcher-item[aria-current="true"]')),
    );
    expect(current.map((r) => r.selectors)).toEqual([['.nd-root .nd-switcher-item[aria-current="true"]']]);
    expect(current[0]?.body).toContain("background-color: var(--lane);");
    expect(current[0]?.body).not.toMatch(/border|outline|box-shadow|accent/);
    const all = cssRules(desktopCss).flatMap((r) => r.selectors);
    expect(all.filter((s) => s.includes('.nd-switcher-item[data-open="true"]'))).toEqual([]);
  });

  it("aba do Alternador neutra e aba atual sem violeta (fundo de pressionado e peso do texto)", () => {
    expect(cssRuleBody(mobileCss, '.nd-mobile-tab[data-variant="switcher"] .nd-mobile-tab-icon')).not.toMatch(/accent|background/);
    const tab = cssRuleBody(desktopCss, '.nd-root .nd-mobile-tab[aria-current="page"]');
    expect(tab).toContain("background-color: var(--state-press);");
    expect(tab).toContain("font-weight: var(--fw-semibold);");
    expect(tab).not.toMatch(/accent|border|outline/);
  });
});

describe("A-13: controles da barra do App com a mesma altura", () => {
  it("095/T03: na barra de título, 28 px para endereço, botões e o menu de tamanho", () => {
    expect(cssRuleBody(appCss, ".nd-app-bar")).toContain("--app-control-h: 28px;");
    const heights = cssRules(appCss).find((r) => r.selectors.includes(".nd-app-bar .nd-app-address"));
    expect(heights?.selectors).toContain(".nd-app-bar .nd-btn");
    expect(heights?.body).toContain("height: var(--app-control-h);");
    expect(cssRuleBody(appCss, ".nd-app-address")).not.toMatch(/height:/);
    // Sem barra de ação acima do conteúdo (decisão 10).
    expect(cssRules(appCss).flatMap((r) => r.selectors).filter((sel) => /nd-app-(run|tabs|panel|console)/.test(sel))).toEqual([]);
  });
});

describe("095/T07 A-03: endereço do App rebaixado e sem borda, nos dois temas", () => {
  it("sem borda, fundo de controle (no titânio, o rebaixado `--chrome-control`), texto centralizado", () => {
    const rules = cssRules(appCss).filter((r) => r.at === null && r.selectors.length === 1 && r.selectors[0] === ".nd-app-bar .nd-app-address");
    const body = rules.map((r) => r.body).join(";");
    expect(body).toContain("border: 0;");
    expect(body).toContain("background: var(--control);");
    expect(cssRuleBody(appCss, ".nd-app-address")).toContain("text-align: center;");
    // Nenhuma regra devolve a borda ao endereço.
    const bordered = cssRules(appCss).filter((r) => r.selectors.some((s) => s.includes("nd-app-address")) && /(?:^|;)\s*border(?:-(?!radius)[a-z]+)?:(?!\s*0;)/.test(r.body));
    expect(bordered).toEqual([]);
    // O titânio troca o `--control` pelo rebaixado do cromo (mesma regra das barras de título).
    expect(cssRuleBody(desktopCss, ".nd-chrome")).toContain("--control: var(--chrome-control);");
  });
});

describe("A-14 (adaptada na 095/T06): a conexão é uma linha, não um cartão de fatos", () => {
  const settingsCss = read("apps/settings/settings.css");
  it("linha com divisória, detalhe em --muted e sem a grade de fatos antiga", () => {
    expect(shellCss).not.toContain(".nd-connection-facts");
    const row = cssRuleBody(settingsCss, ".nd-conn");
    expect(row).toContain("border-bottom: 1px solid var(--divider);");
    expect(row).not.toMatch(/border-radius|border:/);
    expect(cssRuleBody(settingsCss, ".nd-conn-detail")).toContain("color: var(--muted);");
  });
});

describe("A-15: um só tom de perigo", () => {
  it.each(THEME_NAMES)("%s: a pílula rosa e o --danger-ink usam a mesma tinta", (theme) => {
    expect(THEMES[theme].dangerInk).toBe(DANGER_INK[theme]);
    expect(THEMES[theme].pill.rose.ink).toBe(DANGER_INK[theme]);
    const vars = themeVariables(theme);
    expect(vars["--pill-rose-ink"]).toBe(vars["--danger-ink"]);
  });
});
