/*
 * Barra de cima sem quebra nem sobreposição (spec 137/T03, G2), de 1024 a 1920 px, nos dois idiomas.
 *
 * O jsdom não tem layout: a conferência é uma soma das larguras, com as medidas lidas do CSS-fonte e
 * os textos lidos dos dicionários. A largura do texto é estimada ACIMA da real da Inter (0,6 em no
 * peso médio e 0,68 em no semibold; medido no Chromium, a Inter fica perto de 0,54 e 0,64), então a
 * conta tem folga. O pior caso: projeto aberto com nome comprido, Layout, Automático, Internet, o
 * selo "Setup incompleto", a busca, o sino e a pílula com o botão ao lado.
 *
 * Quem cede espaço é a faixa de status: a fase encolhe com reticências (o texto inteiro fica na
 * dica). Da pílula, só se exige a largura mínima legível: o ponto e 8 caracteres da fase.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { DICTIONARIES, type MessageKey } from "../../i18n";
import { LANGS, type Lang } from "../../i18n/lang";
import { cssRuleBody, cssRules } from "../test-utils";

type CssRule = ReturnType<typeof cssRules>[number];

const read = (path: string): string => readFileSync(resolve(process.cwd(), "src", path), "utf8");
const desktopCss = read("desktop/theme/desktop.css");
const shellCss = read("desktop/theme/shell.css");
const SHELL_RULES = cssRules(shellCss);

/** Seletores do desktop (o celular tem a barra compacta própria). */
const D = '.nd-topbar-content:not([data-compact="true"])';
const SEARCH = `${D} .nd-search-btn`;
const SEARCH_TEXT = `${D} .nd-search-btn > .nd-search-text`;
const SEARCH_KBD = `${D} .nd-search-btn > kbd`;
const LAYOUT_TEXT = `${D} [data-layout-menu] > .nd-bar-text`;
const SETUP_TEXT = `${D} [data-setup-badge] > .nd-bar-label`;
const TOGGLE_TEXT = `${D} .nd-toggle > .nd-bar-label`;
const SWITCHER_TEXT = `${D} [data-space-switcher] > .nd-bar-text`;

/** Abaixo desta largura, rótulos podem virar ícone; daqui para cima, os principais ficam visíveis. */
const ALL_LABELS_FROM = 1366;
/** Largura mínima legível da fase na pílula, em caracteres. */
const PHASE_MIN_CHARS = 8;

/** Largura estimada (px) de um texto: acima da largura real da Inter. */
function textWidth(text: string, fontPx: number, semibold = false): number {
  return [...text].length * fontPx * (semibold ? 0.68 : 0.6);
}

function px(body: string, prop: string): number {
  const m = new RegExp(`(?:^|[;\\s])${prop}: (\\d+(?:\\.\\d+)?)px;`).exec(body);
  if (m === null) throw new Error(`${prop} ausente em: ${body}`);
  return Number(m[1]);
}

/** Limite (`max-width` da @media) em que a regra passa a valer; `null` se não há. */
function mediaMax(rule: CssRule): number | null {
  const m = rule.at === null ? null : /^@media \(max-width: (\d+(?:\.\d+)?)px\)$/.exec(rule.at);
  return m === null ? null : Number(m[1]);
}

/** As faixas em que um seletor vale (`max-width` de cada @media). */
function bands(selector: string): Array<{ max: number; body: string }> {
  return SHELL_RULES.filter((r) => r.selectors.includes(selector)).flatMap((r) => {
    const max = mediaMax(r);
    return max === null ? [] : [{ max, body: r.body }];
  });
}

/** Na largura `w`, o elemento do seletor sai (`display: none`)? */
function hidden(selector: string, w: number): boolean {
  return bands(selector).some((b) => w <= b.max && b.body.includes("display: none;"));
}

/** Valor de `prop` na largura `w`: a faixa mais estreita que vale e o define, ou `fallback`. */
function bandPx(selector: string, prop: string, w: number, fallback: number): number {
  const applying = bands(selector).filter((b) => w <= b.max && new RegExp(`(?:^|[;\\s])${prop}:`).test(b.body));
  if (applying.length === 0) return fallback;
  return px(applying.reduce((a, b) => (b.max < a.max ? b : a)).body, prop);
}

/** Maior faixa em que o seletor some (`-Infinity` se nunca some). */
function hiddenFrom(selector: string): number {
  return Math.max(...bands(selector).filter((b) => b.body.includes("display: none;")).map((b) => b.max), -Infinity);
}

function longest(values: readonly string[]): string {
  return values.reduce((a, b) => ([...b].length > [...a].length ? b : a), "");
}

const FS_CONTROL = 13;
const FS_LABEL = 12;
const GAP = 8; // `--sp-2`, entre os controles da barra e dentro do `.nd-btn`
const BTN_PAD = 12; // `.nd-bar-btn`: `0 var(--sp-3)`
const DOT = 7; // `.nd-dot-sm`
const SEARCH_PAD = 12 + 8; // `.nd-search-btn`: `0 var(--sp-2) 0 var(--sp-3)`

function texts(lang: Lang) {
  const dict = DICTIONARIES[lang];
  const tr = (key: MessageKey): string => dict[key];
  const layouts = (["planning", "execution", "done"] as const).map((c) => tr("layout.button").replace("{phase}", tr(`layout.category.${c}` as MessageKey)));
  return {
    layout: longest(layouts),
    auto: tr("autoApprove.short"),
    internet: tr("internet.short"),
    setup: tr("topbar.setupIncomplete"),
    search: tr("topbar.search"),
    pillButton: longest([tr("run.stop"), tr("run.stopping"), tr("run.continue"), tr("run.resume")]),
  };
}

function barWidth(w: number, lang: Lang): { fixed: number; status: number } {
  const x = texts(lang);
  const btn = (inner: number): number => 2 * BTN_PAD + inner;
  const nbtn = px(cssRuleBody(desktopCss, ".nd-nbtn"), "width");
  const iconbtn = px(cssRuleBody(desktopCss, ".nd-iconbtn"), "width");
  const switcherCap = bandPx(SWITCHER_TEXT, "max-width", w, px(cssRuleBody(shellCss, ".nd-bar-text"), "max-width"));
  const switcher = btn(DOT + GAP + switcherCap + GAP + 14);
  const layout = hidden(LAYOUT_TEXT, w) ? btn(16) : btn(16 + GAP + textWidth(x.layout, FS_CONTROL));
  // Interruptor: trilho de 24 px, ícone de 14 e o rótulo.
  const toggle = (label: string): number => (hidden(TOGGLE_TEXT, w) ? btn(24 + GAP + 14) : btn(24 + GAP + 14 + GAP + textWidth(label, FS_CONTROL)));
  const setup = hidden(SETUP_TEXT, w) ? btn(14) : btn(14 + GAP + textWidth(x.setup, FS_CONTROL));
  const search = bandPx(SEARCH, "width", w, px(cssRuleBody(desktopCss, ".nd-search-btn"), "width"));
  const content = [switcher, layout, toggle(x.auto), toggle(x.internet), setup, search, iconbtn];
  // Barra: o N, o conteúdo, Atalhos e Personalizar; padding `0 var(--sp-3) 0 var(--sp-4)`.
  const header = [nbtn, iconbtn, iconbtn];
  // Vãos: 3 entre os 4 filhos da barra e 7 entre os 8 do conteúdo (os 7 controles e o centro).
  const gaps = GAP * header.length + GAP * content.length;
  const fixed = 16 + 12 + header.reduce((a, b) => a + b, 0) + content.reduce((a, b) => a + b, 0) + gaps;
  // Pílula no mínimo legível: padding `0 var(--sp-1) 0 var(--sp-3)`, ponto, 8 caracteres da fase,
  // encaixes do título e dos detalhes (vazios, mas com o vão) e o botão (padding `--sp-2`, ícone de
  // 14, texto de 12 px).
  const phase = PHASE_MIN_CHARS * FS_CONTROL * 0.68;
  const pillButton = 2 * 8 + 14 + GAP + textWidth(x.pillButton, FS_LABEL);
  const status = 12 + 4 + DOT + phase + pillButton + 4 * GAP;
  return { fixed, status };
}

const WIDTHS = Array.from({ length: 1920 - 1024 + 1 }, (_, i) => 1024 + i);

describe("barra de cima sem quebra nem sobreposição (137/T03, G2)", () => {
  it("só a faixa de status encolhe: os outros controles da barra têm `flex-shrink: 0` e não quebram linha", () => {
    expect(cssRuleBody(shellCss, `${D} > :not(.nd-topbar-center)`)).toContain("flex-shrink: 0;");
    expect(cssRuleBody(desktopCss, ".nd-topbar > :not(.nd-topbar-slot)")).toContain("flex-shrink: 0;");
    const center = cssRuleBody(shellCss, ".nd-topbar-center");
    expect(center).toContain("flex: 1 1 0;");
    expect(center).toContain("min-width: 0;");
    expect(cssRuleBody(desktopCss, ".nd-btn")).toContain("white-space: nowrap;");
    expect(cssRuleBody(desktopCss, ".nd-search-btn")).toContain("flex: 0 0 220px;");
    // O nome comprido do projeto tem teto, com reticências.
    const text = cssRuleBody(shellCss, ".nd-bar-text");
    expect(text).toContain("white-space: nowrap;");
    expect(text).toContain("text-overflow: ellipsis;");
  });

  it("a fase encolhe com reticências; o título e os detalhes só ocupam a sobra", () => {
    const phase = cssRuleBody(shellCss, ".nd-run-pill > .nd-run-phase:not(.nd-run-op)");
    expect(phase).toContain("min-width: 0;");
    expect(phase).toContain("overflow: hidden;");
    expect(phase).toContain("text-overflow: ellipsis;");
    for (const slot of [".nd-run-title-slot", ".nd-run-details-slot"]) {
      const body = cssRuleBody(shellCss, slot);
      expect(body).toContain("flex: 1 1 0;");
      expect(body).toContain("max-width: max-content;");
    }
  });

  it(`de ${ALL_LABELS_FROM} px para cima, os rótulos do Layout, de Automático, de Internet e da busca ficam visíveis`, () => {
    for (const s of [LAYOUT_TEXT, TOGGLE_TEXT, SEARCH_TEXT]) expect(hiddenFrom(s)).toBeLessThan(ALL_LABELS_FROM);
    // O nome do projeto nunca some: só tem teto com reticências.
    expect(hiddenFrom(SWITCHER_TEXT)).toBe(-Infinity);
    // A busca, mesmo mais estreita, mostra o rótulo inteiro nos dois idiomas.
    for (const lang of LANGS) {
      for (const w of WIDTHS.filter((v) => v >= ALL_LABELS_FROM)) {
        const width = bandPx(SEARCH, "width", w, px(cssRuleBody(desktopCss, ".nd-search-btn"), "width"));
        const kbd = hidden(SEARCH_KBD, w) ? 0 : GAP + textWidth("Ctrl+K", 11) + 8;
        expect(SEARCH_PAD + 14 + GAP + textWidth(texts(lang).search, FS_CONTROL) + kbd).toBeLessThanOrEqual(width);
      }
    }
  });

  it("abaixo disso, os rótulos saem pelo que menos faz falta: Automático e Internet, depois a busca e o Layout", () => {
    expect(hiddenFrom(TOGGLE_TEXT)).toBeGreaterThan(hiddenFrom(SEARCH_TEXT));
    expect(hiddenFrom(SEARCH_TEXT)).toBeGreaterThan(hiddenFrom(LAYOUT_TEXT));
    for (const s of [SEARCH_TEXT, LAYOUT_TEXT, SETUP_TEXT, TOGGLE_TEXT]) expect(hidden(s, 1920)).toBe(false);
  });

  it.each(LANGS)("em %s, de 1024 a 1920 px, os controles e a pílula no mínimo legível cabem numa linha", (lang) => {
    const tight = WIDTHS.filter((w) => {
      const { fixed, status } = barWidth(w, lang);
      return fixed + status > w;
    });
    expect(tight).toEqual([]);
  });
});
