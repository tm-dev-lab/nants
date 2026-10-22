import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody } from "../test-utils";
import {
  allWallpaperExtremes,
  CHROME_CONTENT_BAND,
  CHROME_SAMPLE_X,
  CHROME_SAMPLE_Y,
  CHROME_SURFACES,
  chromeBackgrounds,
  contrastRatio,
  luminance,
  minContrast,
  parseColor,
  stack,
  titaniumAt,
  titaniumExtremes,
  titaniumSamples,
  wallpaperExtremes,
  wallpaperSamples,
  windowBackgrounds,
  type Rgba,
} from "./contrast";
import {
  CODE_THEMES,
  LIGHT_WINDOW_SEPARATION,
  PHASE_TONE,
  RUN_PILL_TONE,
  THEME_NAMES,
  THEMES,
  TONES,
  WALLPAPER_IDS,
  type CodeTokens,
  type ThemeName,
} from "./tokens";
import { DOCK_HEIGHT } from "../metrics";

const TEXT = 4.5;
const NON_TEXT = 3;

describe("cores e contraste (funções)", () => {
  it("lê hex curto, longo e rgba", () => {
    expect(parseColor("rgb(255, 0, 0)")).toEqual({ r: 255, g: 0, b: 0, a: 1 });
    expect(parseColor("rgba(0, 0, 0, 0.5)").a).toBe(0.5);
    expect(parseColor(THEMES.light.surface)).toEqual({ r: 255, g: 255, b: 255, a: 1 });
    expect(() => parseColor("azul")).toThrow();
  });

  it("razão de contraste: preto sobre branco é 21 e a mesma cor é 1", () => {
    expect(contrastRatio("rgb(0, 0, 0)", "rgb(255, 255, 255)")).toBeCloseTo(21, 5);
    expect(contrastRatio("rgb(10, 20, 30)", "rgb(10, 20, 30)")).toBeCloseTo(1, 5);
  });

  it("compõe camadas translúcidas sobre um fundo opaco", () => {
    const mid = stack("rgb(0, 0, 0)", "rgba(255, 255, 255, 0.5)");
    expect(mid.r).toBeCloseTo(127.5, 5);
    expect(mid.a).toBe(1);
  });

  it("os extremos de cada papel de parede são a mais clara e a mais escura das amostras", () => {
    for (const theme of THEME_NAMES) {
      for (const id of WALLPAPER_IDS) {
        const samples = wallpaperSamples(id, theme);
        const { lightest, darkest } = wallpaperExtremes(id, theme);
        const lums = samples.map(luminance);
        expect(luminance(lightest)).toBeCloseTo(Math.max(...lums), 10);
        expect(luminance(darkest)).toBeCloseTo(Math.min(...lums), 10);
      }
    }
  });
});

describe("titânio com reflexo: extremos na faixa de conteúdo (C-01)", () => {
  it.each(THEME_NAMES)("%s: os extremos são a amostra mais clara e a mais escura da grade, só na faixa de conteúdo", (theme) => {
    const { lightest, darkest } = titaniumExtremes(theme);
    const samples = titaniumSamples(theme);
    const lums = samples.map(luminance);
    expect(luminance(lightest)).toBeCloseTo(Math.max(...lums), 12);
    expect(luminance(darkest)).toBeCloseTo(Math.min(...lums), 12);
    // Opacos (o material não deixa ver o fundo) e com o claro acima do escuro.
    for (const s of samples) expect(s.a).toBeCloseTo(1, 12);
    expect(luminance(lightest)).toBeGreaterThan(luminance(darkest));
    expect(chromeBackgrounds(theme)).toEqual([lightest, darkest]);
    // A grade fica dentro da faixa de conteúdo e a cobre de ponta a ponta.
    expect(Math.min(...CHROME_SAMPLE_Y)).toBe(CHROME_CONTENT_BAND.from);
    expect(Math.max(...CHROME_SAMPLE_Y)).toBe(CHROME_CONTENT_BAND.to);
    // Na largura: as duas pontas, o pico da faixa diagonal (44%) e o centro do brilho (66%).
    for (const x of [0, 0.44, 0.66, 1]) expect(CHROME_SAMPLE_X).toContain(x);
  });

  it("a borda de luz do topo (y = 0, sem conteúdo) é mais clara que tudo na faixa das barras, no escuro", () => {
    const box = CHROME_SURFACES.bar.boxes[0] ?? { w: 1440, h: 44 };
    expect(luminance(titaniumAt("dark", 0.66, 0, box))).toBeGreaterThan(luminance(titaniumExtremes("dark").lightest));
  });

  it("o titânio só tem as superfícies de barra (barras, títulos e dock); as sobreposições são sólidas", () => {
    expect(Object.keys(CHROME_SURFACES)).toEqual(["bar"]);
    for (const theme of THEME_NAMES) {
      const t = THEMES[theme];
      // A sobreposição (`--overlay`) é opaca e entra nos fundos das janelas, com os pares normais.
      expect(parseColor(t.overlay).a).toBe(1);
      expect(windowBackgrounds(theme)).toContainEqual(parseColor(t.overlay));
    }
  });

  it("cada camada tem o alfa real no ponto: o brilho some fora da elipse e a faixa de luz acaba em 52%", () => {
    const ti = THEMES.dark.titanium;
    const box = CHROME_SURFACES.bar.boxes[0] ?? { w: 1440, h: 44 };
    // Em 60% da altura, longe do brilho (x = 0) e da faixa diagonal: só a base e a sombra da faixa.
    const plain = titaniumAt("dark", 0, 0.6, box);
    const baseAt60 = stack(ti.base.mid, { ...parseColor(ti.base.bottom), a: (0.6 - 0.52) / 0.48 });
    const shade = parseColor(ti.band.shade);
    expect(luminance(plain)).toBeCloseTo(luminance(stack(baseAt60, { ...shade, a: (shade.a * (0.6 - 0.52)) / 0.48 })), 4);
  });

  it("o reflexo do escuro nunca fica abaixo do mockup (Cromo.dc.html, Titânio com reflexo)", () => {
    const ti = THEMES.dark.titanium;
    expect([ti.base.top, ti.base.mid, ti.base.bottom]).toEqual(["#3A3A46", "#2B2B35", "#212129"]);
    expect(parseColor(ti.band.top).a).toBeGreaterThanOrEqual(0.16);
    expect(parseColor(ti.band.mid).a).toBeGreaterThanOrEqual(0.06);
    expect(parseColor(ti.glint).a).toBeGreaterThanOrEqual(0.14);
    expect(parseColor(ti.streak.peak).a).toBeGreaterThanOrEqual(0.1);
    expect(parseColor(ti.streak.tail).a).toBeGreaterThanOrEqual(0.02);
  });

  it("o botão do dock em foco tem o fundo --state-press, e hover e pressionado não empilham outra camada", () => {
    const css = readFileSync(resolve(process.cwd(), "src/desktop/theme/desktop.css"), "utf8");
    expect(cssRuleBody(css, '.nd-dock-btn[data-state="focused"]')).toContain("background-color: var(--state-press);");
    const reset = cssRuleBody(css, '.nd-root .nd-dock-btn[data-state="focused"]:hover');
    expect(reset).toContain("background-image: none;");
    expect(css.indexOf('.nd-root .nd-dock-btn[data-state="focused"]:hover')).toBeGreaterThan(css.indexOf(".nd-root .nd-dock-btn:active {"));
  });

  it("geometria: os controles das barras são centralizados e cabem na faixa de conteúdo", () => {
    const css = readFileSync(resolve(process.cwd(), "src/desktop/theme/desktop.css"), "utf8");
    for (const selector of [".nd-topbar", ".nd-titlebar", ".nd-dock", ".nd-dock-btn"]) {
      expect({ selector, body: cssRuleBody(css, selector) }).toEqual({ selector, body: expect.stringContaining("align-items: center;") });
    }
    // Ícones de 16 a 20 px e texto de 11 a 14 px em barras de 40 a 44 px; ícone de 20 px no dock de 60 px (botão de 44, 095/T02).
    const cases: [number, number][] = [];
    for (const bar of [40, 44]) for (const size of [11, 12, 13, 14, 16, 18, 20]) cases.push([bar, size]);
    cases.push([DOCK_HEIGHT, 20]);
    for (const [bar, size] of cases) {
      const top = (bar - size) / 2 / bar;
      const bottom = (bar + size) / 2 / bar;
      expect({ bar, size, fits: top >= CHROME_CONTENT_BAND.from && bottom <= CHROME_CONTENT_BAND.to }).toEqual({ bar, size, fits: true });
    }
  });

  it.each(THEME_NAMES)("%s: as camadas do reflexo são translúcidas e neutras; a base é opaca", (theme) => {
    const ti = THEMES[theme].titanium;
    for (const layer of [ti.band.top, ti.band.mid, ti.band.shade, ti.glint, ti.streak.peak, ti.streak.tail, ti.edgeTop, ti.edgeBottom]) {
      const c = parseColor(layer);
      expect(c.a).toBeLessThan(1);
      expect(Math.max(c.r, c.g, c.b) - Math.min(c.r, c.g, c.b)).toBeLessThanOrEqual(10);
    }
    for (const stop of [ti.base.top, ti.base.mid, ti.base.bottom]) expect(parseColor(stop).a).toBe(1);
    // A base desce do topo para baixo (luz vinda de cima).
    const l = [ti.base.top, ti.base.mid, ti.base.bottom].map((c) => luminance(parseColor(c)));
    expect(l[0]).toBeGreaterThan(l[1] ?? 0);
    expect(l[1]).toBeGreaterThan(l[2] ?? 0);
  });
});

/** Uma linha da tabela de pares: token de frente, fundos e mínimo exigido. */
interface Pair {
  label: string;
  fg: string;
  backgrounds: Rgba[];
  min: number;
}

function pairsFor(theme: ThemeName): Pair[] {
  const t = THEMES[theme];
  const windowBg = windowBackgrounds(theme);
  // Titânio: barra superior, barras de título, dock, abas do celular e segmentados.
  const chromeBg = chromeBackgrounds(theme);
  const pairs: Pair[] = [];
  const text = (label: string, fg: string, backgrounds: Rgba[]): void => {
    pairs.push({ label, fg, backgrounds, min: TEXT });
  };
  const nonText = (label: string, fg: string, backgrounds: Rgba[]): void => {
    pairs.push({ label, fg, backgrounds, min: NON_TEXT });
  };

  // Texto nas janelas (sólidas).
  for (const [name, fg] of [
    ["ink", t.ink],
    ["ink2", t.ink2],
    ["ink3", t.ink3],
    ["muted", t.muted],
  ] as const) {
    text(`${name} na janela`, fg, windowBg);
  }
  // Texto no titânio: dentro de `.nd-chrome` `--ink`/`--ink2`/`--ink3` valem `--chrome-ink` e `--muted` vale `--chrome-muted`.
  text("chrome-ink no titânio", t.chromeInk, chromeBg);
  text("chrome-muted no titânio", t.chromeMuted, chromeBg);
  // As cores do cromo também valem para os filhos do titânio que pintam a superfície da janela.
  text("chrome-ink na janela", t.chromeInk, windowBg);
  text("chrome-muted na janela", t.chromeMuted, windowBg);
  // Ação principal.
  text("texto da ação principal", t.accentInk, [parseColor(t.accent)]);
  // Ação principal desabilitada (pendência da T09): texto --muted sobre --surface2, ≥ 3:1.
  nonText("texto da ação principal desabilitada", t.muted, [parseColor(t.surface2)]);
  // Ícones: nas janelas, os tons; no titânio (dock e barras), os tons do cromo, também nos estados do dock.
  for (const tone of TONES) {
    const icon = t.chromeIcon[tone];
    nonText(`ícone ${tone} na janela`, t.icon[tone], windowBg);
    nonText(`ícone ${tone} no titânio`, icon, chromeBg);
    nonText(`ícone ${tone} do dock em hover`, icon, chromeBg.map((bg) => stack(bg, t.stateHover)));
    nonText(`ícone ${tone} do dock pressionado`, icon, chromeBg.map((bg) => stack(bg, t.statePress)));
    // Janela em foco: o fundo `--state-press` do botão (hover e pressionado não empilham, teste abaixo).
    nonText(`ícone ${tone} do dock em foco`, icon, chromeBg.map((bg) => stack(bg, t.statePress)));
    // Os tons do cromo também valem nos filhos do titânio que pintam a superfície da janela.
    nonText(`ícone ${tone} do cromo na janela`, icon, windowBg);
  }
  // Botões da barra de título (ícone `--muted`, que no titânio é o `--chrome-muted`) e controles.
  nonText("botões da barra de título", t.chromeMuted, chromeBg);
  nonText("borda de controle", t.controlEdge, windowBg);
  // No titânio a borda de controle (`kbd`, campos dos popovers) é o `--chrome-muted`.
  nonText("borda de controle no titânio", t.chromeMuted, chromeBg);
  // Contraste alto: o titânio vira `--surface2` sólido e o contorno é o `--edge-strong`.
  nonText("borda forte (alto contraste)", t.edgeStrong, [...windowBg, parseColor(t.surface2)]);
  // Indicador de seleção (item escolhido, janela em foco no dock): na janela e, com o violeta do cromo, no titânio.
  nonText("indicador de seleção na janela", t.selectedEdge, windowBg);
  nonText("indicador de seleção no titânio", t.chromeInk, [...chromeBg, ...chromeBg.map((bg) => stack(bg, t.statePress))]);
  nonText("indicador de seleção do cromo na janela", t.chromeIcon.violet, windowBg);
  // Pílulas: texto sobre o fundo da pílula composto no titânio da barra superior e na janela.
  for (const tone of TONES) {
    const pill = t.pill[tone];
    text(`pílula ${tone} no titânio`, t.chromePillInk[tone], chromeBg.map((bg) => stack(bg, pill.bg)));
    text(`pílula ${tone} na janela`, pill.ink, windowBg.map((bg) => stack(bg, pill.bg)));
  }
  // Estados comuns dos controles (094/T06): hover, pressionado e selecionado, na janela e no titânio.
  for (const [name, layer] of [
    ["hover", t.stateHover],
    ["pressionado", t.statePress],
  ] as const) {
    text(`texto de controle em ${name} na janela`, t.ink2, windowBg.map((bg) => stack(bg, layer)));
    text(`item de menu em ${name} no titânio`, t.chromeInk, chromeBg.map((bg) => stack(bg, layer)));
    nonText(`borda de controle em ${name}`, t.controlEdge, windowBg.map((bg) => stack(bg, layer)));
    nonText(`botão da janela em ${name}`, t.chromeMuted, chromeBg.map((bg) => stack(bg, layer)));
  }
  // Variantes com cor própria sob a camada de estado: ação principal e perigo (botão e fechar).
  for (const [name, layer] of [
    ["hover", t.stateHover],
    ["pressionado", t.statePress],
  ] as const) {
    text(`texto da ação principal em ${name}`, t.accentInk, [stack(t.accent, layer)]);
    text(`texto de perigo em ${name} na janela`, t.dangerInk, windowBg.map((bg) => stack(bg, t.dangerSoft, layer)));
    // Sobre o cromo o `--danger-ink` é a tinta rosa do cromo.
    nonText(`ícone de fechar em ${name} na barra de título`, t.chromePillInk.rose, chromeBg.map((bg) => stack(bg, t.dangerSoft, layer)));
  }
  text("controle selecionado na janela", t.ink, windowBg.map((bg) => stack(bg, t.accentSoft)));
  text("item selecionado no titânio", t.chromeInk, chromeBg.map((bg) => stack(bg, t.accentSoft)));
  text("item selecionado em hover no titânio", t.chromeInk, chromeBg.map((bg) => stack(bg, t.accentSoft, t.stateHover)));
  // Controle segmentado (titânio): segmento comum e escolhido.
  text("segmento no titânio", t.chromeInk, chromeBg);
  text("segmento escolhido no titânio", t.chromeInk, chromeBg.map((bg) => stack(bg, t.accentSoft)));
  // Selo do dock.
  text("selo do dock", t.badgeInk, [parseColor(t.badgeBg)]);
  // Fechar em hover.
  nonText("ícone de fechar em hover", t.chromePillInk.rose, chromeBg.map((bg) => stack(bg, t.dangerSoft)));
  text("texto de perigo no titânio", t.chromePillInk.rose, chromeBg);

  // --- 095/T02: botões neutros, pílulas de fundo neutro, `<kbd>` sem serifa, busca e dock neutro ---
  // Nas janelas e sobreposições: fundo `--control` (secundária, pílulas, `<kbd>`), com hover e pressionado.
  const controlBg = windowBg.map((bg) => stack(bg, t.control));
  text("botão secundário na janela", t.ink, controlBg);
  for (const [name, layer] of [
    ["hover", t.stateHover],
    ["pressionado", t.statePress],
  ] as const) {
    text(`botão secundário em ${name} na janela`, t.ink, controlBg.map((bg) => stack(bg, layer)));
    text(`botão fantasma em ${name} na janela`, t.ink2, windowBg.map((bg) => stack(bg, layer)));
  }
  text("atalho (<kbd>) na janela", t.muted, controlBg);
  nonText("texto da primária desabilitada (fundo neutro)", t.muted, controlBg);
  for (const tone of TONES) text(`pílula de estado ${tone} (fundo neutro) na janela`, t.pill[tone].ink, controlBg);
  text("perigo (Remover) na janela", t.dangerInk, windowBg.map((bg) => stack(bg, t.dangerSoft)));
  // Sobre o titânio: dentro de `.nd-chrome` o `--control` é o `--chrome-control`.
  const chromeControlBg = chromeBg.map((bg) => stack(bg, t.chromeControl));
  text("botão secundário no titânio (seletor de projeto, Setup)", t.chromeInk, chromeControlBg);
  for (const [name, layer] of [
    ["hover", t.stateHover],
    ["pressionado", t.statePress],
  ] as const) {
    text(`botão secundário em ${name} no titânio`, t.chromeInk, chromeControlBg.map((bg) => stack(bg, layer)));
    text(`busca (--muted) em ${name} no titânio`, t.chromeMuted, chromeControlBg.map((bg) => stack(bg, layer)));
    nonText(`pino do interruptor ligado em ${name} no titânio`, t.chromeIcon.amber, chromeBg.map((bg) => stack(bg, layer)));
    nonText(`pino do interruptor (Internet) em ${name} no titânio`, t.chromeIcon.violet, chromeBg.map((bg) => stack(bg, layer)));
  }
  text("busca: 'Buscar ou pedir' (--muted) no titânio", t.chromeMuted, chromeControlBg);
  // O `<kbd>` da busca é um fundo neutro sobre o fundo neutro do botão.
  text("atalho (<kbd>) da busca no titânio", t.chromeMuted, chromeControlBg.map((bg) => stack(bg, t.chromeControl)));
  // Pílula de execução: fundo neutro; o tom só no ponto e no rótulo da fase.
  for (const tone of TONES) {
    text(`rótulo da fase ${tone} na pílula (fundo neutro) no titânio`, t.chromePillInk[tone], chromeControlBg);
    nonText(`ponto ${tone} na pílula no titânio`, t.chromeIcon[tone], chromeControlBg);
  }
  text("título da spec na pílula no titânio", t.chromeInk, chromeControlBg);
  text("detalhes (--muted) na pílula no titânio", t.chromeMuted, chromeControlBg);
  // Parar: perigo rosa suave dentro da pílula (sobre o fundo neutro dela), com hover.
  const stopBg = chromeControlBg.map((bg) => stack(bg, t.dangerSoft));
  text("Parar (perigo) na pílula no titânio", t.chromePillInk.rose, stopBg);
  text("Parar (perigo) em hover na pílula no titânio", t.chromePillInk.rose, stopBg.map((bg) => stack(bg, t.stateHover)));
  // Dock neutro: aberta `--ink` (no cromo, `--chrome-ink`), fechada `--muted`; também em hover, pressionado e em foco.
  for (const [name, fg] of [
    ["aberta", t.chromeInk],
    ["fechada", t.chromeMuted],
  ] as const) {
    nonText(`ícone do dock (janela ${name})`, fg, chromeBg);
    nonText(`ícone do dock (janela ${name}) em hover`, fg, chromeBg.map((bg) => stack(bg, t.stateHover)));
    nonText(`ícone do dock (janela ${name}) pressionado ou em foco`, fg, chromeBg.map((bg) => stack(bg, t.statePress)));
  }
  nonText("ponto da janela aberta no dock", t.chromeInk, chromeBg.map((bg) => stack(bg, t.statePress)));

  // Sobreposições (menus, popovers, sino, lançador, avisos e dicas): superfície sólida `--overlay`,
  // já em `windowBackgrounds`; aqui, explícitos, os pares de menu com os tokens normais.
  const overlayBg = [parseColor(t.overlay)];
  text("item de menu na sobreposição", t.ink2, overlayBg);
  text("rótulo do menu (secundário) na sobreposição", t.muted, overlayBg);
  for (const [name, layer] of [
    ["hover", t.stateHover],
    ["pressionado", t.statePress],
  ] as const) {
    const states = overlayBg.map((bg) => stack(bg, layer));
    text(`item de menu em ${name} na sobreposição`, t.ink2, states);
    text(`secundário em ${name} na sobreposição`, t.muted, states);
    for (const tone of TONES) nonText(`ícone ${tone} em ${name} na sobreposição`, t.icon[tone], states);
  }
  return pairs;
}

function focusRingOk(theme: ThemeName, bg: Rgba): number {
  const t = THEMES[theme];
  return Math.max(contrastRatio(t.focusInner, bg), contrastRatio(t.focusOuter, bg));
}

describe.each(THEME_NAMES)("contraste dos pares de tokens: tema %s", (theme) => {
  it.each(pairsFor(theme).map((p) => [p.label, p] as const))("%s", (_label, pair) => {
    expect(minContrast(pair.fg, pair.backgrounds)).toBeGreaterThanOrEqual(pair.min);
  });

  it("anel de foco duplo: um dos anéis ≥ 3:1 em qualquer fundo, e os dois se distinguem", () => {
    const t = THEMES[theme];
    const backgrounds = [...windowBackgrounds(theme), ...chromeBackgrounds(theme), ...allWallpaperExtremes(theme).map((e) => e.color)];
    for (const bg of backgrounds) expect(focusRingOk(theme, bg)).toBeGreaterThanOrEqual(NON_TEXT);
    expect(contrastRatio(t.focusInner, t.focusOuter)).toBeGreaterThanOrEqual(NON_TEXT);
  });

  it("tema do CodeMirror: texto e realces ≥ 4,5:1 sobre as superfícies da janela", () => {
    const code = CODE_THEMES[theme];
    const keys: (keyof CodeTokens)[] = [
      "text",
      "gutter",
      "keyword",
      "string",
      "number",
      "comment",
      "function",
      "type",
      "property",
      "tag",
      "operator",
      "link",
    ];
    for (const key of keys) {
      expect({ key, ratio: minContrast(code[key], windowBackgrounds(theme)) >= TEXT }).toEqual({ key, ratio: true });
    }
    expect(minContrast(code.focus, windowBackgrounds(theme))).toBeGreaterThanOrEqual(NON_TEXT);
  });
});

describe("tema claro: dock e pílula de execução com as variantes escuras da marca", () => {
  const light = THEMES.light;

  it("os ícones do dock no claro usam as variantes escuras (ciano, violeta, âmbar)", () => {
    const dark = THEMES.dark;
    expect(light.icon.cyan).not.toBe(dark.icon.cyan);
    expect(light.icon.cyan).toBe(light.pill.cyan.ink);
    expect(light.icon.violet).toBe(light.accent);
    // As três variantes escuras declaradas na decisão 13 (no titânio claro, o ciano escurece um passo).
    for (const tone of ["cyan", "violet", "amber"] as const) {
      expect(luminance(parseColor(light.icon[tone]))).toBeLessThan(luminance(parseColor(dark.icon[tone])));
      expect(luminance(parseColor(light.chromeIcon[tone]))).toBeLessThanOrEqual(luminance(parseColor(light.icon[tone])));
      expect(minContrast(light.icon[tone], chromeBackgrounds("light"))).toBeGreaterThanOrEqual(NON_TEXT);
    }
  });

  it("a pílula de execução no claro tem texto ≥ 4,5:1 sobre o titânio da barra", () => {
    const pill = light.pill[RUN_PILL_TONE];
    const backgrounds = chromeBackgrounds("light").map((bg) => stack(bg, pill.bg));
    expect(minContrast(pill.ink, backgrounds)).toBeGreaterThanOrEqual(TEXT);
    // O botão Parar dentro dela (tom rosa).
    const stop = light.pill.rose;
    expect(minContrast(stop.ink, backgrounds.map((bg) => stack(bg, stop.bg)))).toBeGreaterThanOrEqual(TEXT);
  });

  it("toda fase tem um tom de pílula", () => {
    for (const tone of Object.values(PHASE_TONE)) expect(TONES).toContain(tone);
  });
});

describe("tema claro: a janela branca se separa dos 4 papéis de parede (C-13)", () => {
  it("o limiar declarado é o do #EEEEF4 (teto da base dos papéis claros), arredondado para baixo", () => {
    const ceiling = contrastRatio(THEMES.light.surface, "#EEEEF4");
    expect(LIGHT_WINDOW_SEPARATION).toBeLessThanOrEqual(ceiling);
    expect(ceiling - LIGHT_WINDOW_SEPARATION).toBeLessThan(0.01);
  });

  it.each(WALLPAPER_IDS)("%s: --surface contra a cor mais clara do papel ≥ o limiar", (id) => {
    const { lightest } = wallpaperExtremes(id, "light");
    expect(contrastRatio(THEMES.light.surface, lightest)).toBeGreaterThanOrEqual(LIGHT_WINDOW_SEPARATION);
  });
});
