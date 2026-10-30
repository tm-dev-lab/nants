/*
 * Contraste WCAG dos tokens (spec 093, decisão 13). Funções puras: ler uma cor de token,
 * compor camadas translúcidas, luminância relativa e razão de contraste. Também calcula as
 * amostras de cada papel de parede (a cor mais clara e a mais escura) e os extremos do titânio
 * com reflexo (095/T01, C-01): o ponto mais claro e o mais escuro do material.
 */
import { THEMES, TITANIUM_GEOMETRY, WALLPAPER_IDS, WALLPAPERS, type ThemeName, type WallpaperId } from "./tokens";

export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

const HEX = /^#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const RGB = /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/i;

/** Lê `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()` ou `rgba()`. Inválida → erro. */
export function parseColor(value: string): Rgba {
  const text = value.trim();
  const hex = HEX.exec(text)?.[1];
  if (hex !== undefined) {
    const full = hex.length <= 4 ? [...hex].map((c) => c + c).join("") : hex;
    const channel = (i: number): number => parseInt(full.slice(i * 2, i * 2 + 2), 16);
    return { r: channel(0), g: channel(1), b: channel(2), a: full.length === 8 ? channel(3) / 255 : 1 };
  }
  const rgb = RGB.exec(text);
  if (rgb !== null) {
    return {
      r: Number(rgb[1]),
      g: Number(rgb[2]),
      b: Number(rgb[3]),
      a: rgb[4] === undefined ? 1 : Number(rgb[4]),
    };
  }
  throw new Error(`cor inválida: ${value}`);
}

/** `top` composto sobre `bottom` (alfa "over"); o resultado é opaco se `bottom` for. */
function over(top: Rgba, bottom: Rgba): Rgba {
  const a = top.a + bottom.a * (1 - top.a);
  if (a === 0) return { r: 0, g: 0, b: 0, a: 0 };
  const mix = (t: number, b: number): number => (t * top.a + b * bottom.a * (1 - top.a)) / a;
  return { r: mix(top.r, bottom.r), g: mix(top.g, bottom.g), b: mix(top.b, bottom.b), a };
}

/** Compõe as camadas (de baixo para cima) sobre uma base. */
export function stack(base: string | Rgba, ...layers: (string | Rgba)[]): Rgba {
  const toRgba = (c: string | Rgba): Rgba => (typeof c === "string" ? parseColor(c) : c);
  return layers.reduce<Rgba>((acc, layer) => over(toRgba(layer), acc), toRgba(base));
}

/** Luminância relativa (WCAG 2.x) de uma cor opaca. */
export function luminance(color: Rgba): number {
  const lin = (c: number): number => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(color.r) + 0.7152 * lin(color.g) + 0.0722 * lin(color.b);
}

/** Razão de contraste entre `fg` (composto sobre `bg`, se translúcido) e `bg` opaco. */
export function contrastRatio(fg: string | Rgba, bg: string | Rgba): number {
  const back = typeof bg === "string" ? parseColor(bg) : bg;
  if (back.a < 1) throw new Error("o fundo do contraste precisa ser opaco");
  const front = stack(back, fg);
  const l1 = luminance(front);
  const l2 = luminance(back);
  return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
}

/** Lado da grade de amostragem dos brilhos radiais (coordenadas normalizadas 0..1). */
const RADIAL_GRID = 80;
/** Os brilhos vão da cor no centro a transparente em 70% do raio (`wallpaperStyle`). */
const RADIAL_FADE = 0.7;

function percentPair(value: string): [number, number] {
  const [a, b] = value.split(/\s+/).map((part) => Number.parseFloat(part) / 100);
  return [a ?? 0, b ?? a ?? 0];
}

/**
 * Cores que aparecem num papel de parede: a base; os pontos sobre a base; as paradas dos
 * gradientes lineares (os extremos de uma interpolação estão nas paradas); e os brilhos radiais
 * amostrados numa grade normalizada, com a sobreposição real entre eles (o raio em % escala com
 * a área, então a grade 0..1 vale para qualquer proporção de tela).
 */
export function wallpaperSamples(id: WallpaperId, theme: ThemeName): Rgba[] {
  const wall = WALLPAPERS[id][theme];
  const base = parseColor(wall.base);
  const samples: Rgba[] = [base];
  const radials: { color: Rgba; cx: number; cy: number; rx: number; ry: number }[] = [];
  for (const layer of wall.layers) {
    if (layer.kind === "linear") samples.push(...layer.stops.map((s) => stack(base, s)));
    else if (layer.kind === "dots") samples.push(stack(base, layer.color));
    else {
      const [cx, cy] = percentPair(layer.at);
      const [rx, ry] = percentPair(layer.size);
      radials.push({ color: parseColor(layer.color), cx, cy, rx, ry });
    }
  }
  if (radials.length > 0) {
    for (let i = 0; i <= RADIAL_GRID; i += 1) {
      for (let j = 0; j <= RADIAL_GRID; j += 1) {
        const u = i / RADIAL_GRID;
        const v = j / RADIAL_GRID;
        const layers = radials.map((g) => {
          const d = Math.hypot((u - g.cx) / g.rx, (v - g.cy) / g.ry);
          const alpha = d >= RADIAL_FADE ? 0 : g.color.a * (1 - d / RADIAL_FADE);
          return { ...g.color, a: alpha };
        });
        // As camadas estão de baixo para cima (como em `WALLPAPERS`).
        samples.push(stack(base, ...layers));
      }
    }
  }
  return samples;
}

/** A cor mais clara e a mais escura do papel de parede. */
export function wallpaperExtremes(id: WallpaperId, theme: ThemeName): { lightest: Rgba; darkest: Rgba } {
  const samples = wallpaperSamples(id, theme);
  const sorted = [...samples].sort((a, b) => luminance(a) - luminance(b));
  const darkest = sorted[0] ?? parseColor(WALLPAPERS[id][theme].base);
  const lightest = sorted[sorted.length - 1] ?? darkest;
  return { lightest, darkest };
}

/** Extremos de todos os papéis de parede de um tema. */
export function allWallpaperExtremes(theme: ThemeName): { id: WallpaperId; which: "lightest" | "darkest"; color: Rgba }[] {
  return WALLPAPER_IDS.flatMap((id) => {
    const { lightest, darkest } = wallpaperExtremes(id, theme);
    return [
      { id, which: "lightest" as const, color: lightest },
      { id, which: "darkest" as const, color: darkest },
    ];
  });
}

/**
 * Faixa vertical onde fica o conteúdo das BARRAS de titânio (fração da altura). Os controles das
 * barras (barra superior e títulos) são centralizados verticalmente: ícone de 16 a 20 px e
 * texto de 11 a 14 px em barra de 40 a 44 px ocupam no máximo de 27% a 73% da altura. Fora dela (a borda de luz no topo, a sombra embaixo) não
 * há texto nem ícone, então o contraste (WCAG: contra o fundo efetivamente atrás do conteúdo) é
 * medido só aqui (095/T01, decisão do Arquiteto).
 */
export const CHROME_CONTENT_BAND = { from: 0.25, to: 0.75 } as const;

/** Grade de amostragem das barras: alturas dentro da faixa de conteúdo e larguras de 0 a 100%. */
export const CHROME_SAMPLE_Y: readonly number[] = [0.25, 0.375, 0.5, 0.625, 0.75];
export const CHROME_SAMPLE_X: readonly number[] = [0, 0.3, 0.44, 0.5, 0.66, 1];

/** Caixa (px) de uma superfície de titânio. */
interface ChromeBox {
  w: number;
  h: number;
}

/**
 * Superfícies de titânio e onde o conteúdo fica nelas. O titânio fica só nas barras (barra
 * superior, barras de título, abas do celular) e nos segmentados, todos com controles
 * centralizados: faixa de 25% a 75% (`CHROME_SAMPLE_Y`). As caixas são proporções reais (px): a
 * linha da faixa diagonal de 100° depende delas. As sobreposições não são titânio (decisão do
 * Arquiteto na revisão 1 da T01): são a superfície sólida `--overlay`, medida em `windowBackgrounds`.
 */
type ChromeSurface = "bar";

export const CHROME_SURFACES: Readonly<Record<ChromeSurface, { boxes: readonly ChromeBox[]; sampleY: (box: ChromeBox) => number[] }>> = {
  bar: {
    // Barra superior e barra de título.
    boxes: [
      { w: 1440, h: 44 },
      { w: 560, h: 44 },
      { w: 600, h: 60 },
    ],
    sampleY: () => [...CHROME_SAMPLE_Y],
  },
};

const pct = (value: string): number => Number.parseFloat(value) / 100;
const TRANSPARENT_RGBA: Rgba = { r: 0, g: 0, b: 0, a: 0 };

/** Cor de um gradiente linear na posição `t` (0..1), interpolada em alfa pré-multiplicado (como o CSS). */
function gradientAt(stops: readonly { at: number; color: Rgba }[], t: number): Rgba {
  const first = stops[0];
  const last = stops[stops.length - 1];
  if (first === undefined || last === undefined) return TRANSPARENT_RGBA;
  if (t <= first.at) return first.color;
  for (let i = 1; i < stops.length; i += 1) {
    const a = stops[i - 1];
    const b = stops[i];
    if (a === undefined || b === undefined || t > b.at) continue;
    const k = b.at === a.at ? 1 : (t - a.at) / (b.at - a.at);
    const alpha = a.color.a + (b.color.a - a.color.a) * k;
    if (alpha === 0) return TRANSPARENT_RGBA;
    const mix = (x: number, y: number): number => (x * a.color.a + (y * b.color.a - x * a.color.a) * k) / alpha;
    return { r: mix(a.color.r, b.color.r), g: mix(a.color.g, b.color.g), b: mix(a.color.b, b.color.b), a: alpha };
  }
  return last.color;
}

/**
 * Posição (0..1) do ponto (x, y) na linha de um gradiente linear de `angle` graus numa caixa w×h,
 * como no CSS: a linha passa pelo centro e o comprimento é |w·sen| + |h·cos|.
 */
function linearPosition(angle: number, box: { w: number; h: number }, x: number, y: number): number {
  const rad = (angle * Math.PI) / 180;
  const dx = Math.sin(rad);
  const dy = -Math.cos(rad);
  const length = Math.abs(box.w * dx) + Math.abs(box.h * dy);
  return ((x - 0.5) * box.w * dx + (y - 0.5) * box.h * dy) / length + 0.5;
}

/**
 * Cor do titânio no ponto (x, y) de uma caixa (frações da largura e da altura). As camadas são as
 * de `titaniumCss`, cada uma com o alfa real naquele ponto: a base interpolada; a faixa diagonal de
 * 100° (posição na linha do gradiente da caixa); o ponto de brilho (queda radial pela elipse
 * 45%×260% em 66%/0%, some em 65%); e a faixa de luz com a sombra (interpolação das paradas).
 */
export function titaniumAt(theme: ThemeName, x: number, y: number, box: ChromeBox): Rgba {
  const ti = THEMES[theme].titanium;
  const g = TITANIUM_GEOMETRY;
  const base = gradientAt(
    [
      { at: 0, color: parseColor(ti.base.top) },
      { at: pct(ti.base.midAt), color: parseColor(ti.base.mid) },
      { at: 1, color: parseColor(ti.base.bottom) },
    ],
    y,
  );
  const band = gradientAt(
    [
      { at: 0, color: parseColor(ti.band.top) },
      { at: pct(ti.band.midAt), color: parseColor(ti.band.mid) },
      { at: pct(ti.band.clearAt), color: TRANSPARENT_RGBA },
      { at: 1, color: parseColor(ti.band.shade) },
    ],
    y,
  );
  const [rx = 1, ry = 1] = g.glint.size.split(" ").map(pct);
  const [cx = 0, cy = 0] = g.glint.at.split(" ").map(pct);
  const glint = parseColor(ti.glint);
  const d = Math.hypot((x - cx) / rx, (y - cy) / ry);
  const glintAlpha = glint.a * Math.max(0, 1 - d / pct(g.glint.fade));
  const streak = gradientAt(
    [
      { at: pct(g.streak.from), color: TRANSPARENT_RGBA },
      { at: pct(g.streak.peak), color: parseColor(ti.streak.peak) },
      { at: pct(g.streak.tail), color: parseColor(ti.streak.tail) },
      { at: pct(g.streak.to), color: TRANSPARENT_RGBA },
    ],
    linearPosition(g.streak.angle, box, x, y),
  );
  return stack(base, streak, { ...glint, a: glintAlpha }, band);
}

/**
 * Todas as amostras do titânio onde há conteúdo num tipo de superfície: as alturas do tipo
 * (`CHROME_SURFACES`) × `CHROME_SAMPLE_X` em cada caixa, mais o pico da faixa diagonal em cada
 * altura (onde ela passa exatamente, que a grade de x poderia pular).
 */
export function titaniumSamples(theme: ThemeName, surface: ChromeSurface = "bar"): Rgba[] {
  const g = TITANIUM_GEOMETRY;
  const kind = CHROME_SURFACES[surface];
  return kind.boxes.flatMap((box) =>
    kind.sampleY(box).flatMap((y) => {
      // x em que a linha do gradiente passa pelo pico, nesta altura (busca binária: a posição cresce com x).
      let lo = 0;
      let hi = 1;
      for (let i = 0; i < 40; i += 1) {
        const mid = (lo + hi) / 2;
        if (linearPosition(g.streak.angle, box, mid, y) < pct(g.streak.peak)) lo = mid;
        else hi = mid;
      }
      return [...CHROME_SAMPLE_X, lo].map((x) => titaniumAt(theme, x, y, box));
    }),
  );
}

/**
 * Extremos do titânio com reflexo (095/T01, C-01): a amostra mais clara e a mais escura da grade
 * onde há conteúdo no tipo de superfície. O material é opaco, então não depende do que está atrás.
 * Substitui o vidro e o metal da 094 (`glassBackgrounds`, `titleBarBackgrounds`, `materialVariants`).
 */
export function titaniumExtremes(theme: ThemeName, surface: ChromeSurface = "bar"): { lightest: Rgba; darkest: Rgba } {
  const sorted = [...titaniumSamples(theme, surface)].sort((a, b) => luminance(a) - luminance(b));
  const darkest = sorted[0] ?? parseColor(THEMES[theme].titanium.base.mid);
  const lightest = sorted[sorted.length - 1] ?? darkest;
  return { lightest, darkest };
}

/** Os dois extremos do titânio de um tipo de superfície como lista de fundos (para `minContrast`). */
export function chromeBackgrounds(theme: ThemeName, surface: ChromeSurface = "bar"): Rgba[] {
  const { lightest, darkest } = titaniumExtremes(theme, surface);
  return [lightest, darkest];
}

/**
 * Fundos opacos do conteúdo das janelas e das sobreposições (`--overlay`: menus, popovers, lançador,
 * avisos e dicas, sólidos como no mockup). Os pares normais valem para todos.
 */
export function windowBackgrounds(theme: ThemeName): Rgba[] {
  const t = THEMES[theme];
  return [parseColor(t.surface), parseColor(t.surface2), parseColor(t.card), parseColor(t.overlay)];
}

/** Menor contraste de `fg` sobre a lista de fundos. */
export function minContrast(fg: string | Rgba, backgrounds: readonly Rgba[]): number {
  return Math.min(...backgrounds.map((bg) => contrastRatio(fg, bg)));
}
