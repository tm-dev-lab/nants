/*
 * Tokens visuais do desktop (spec 093, decisão 13). ÚNICA fonte de cor da interface:
 * nenhum outro arquivo de `web/src` tem hex, `rgb()` ou classe de paleta
 * Tailwind (checagem automatizada em `checks.test.ts`). Daqui saem:
 * - as variáveis CSS de cada tema (`themeVariables`), aplicadas na raiz do desktop;
 * - os papéis de parede (Pontos, Nebulosa, Aurora e Liso), com versão por tema;
 * - o material "titânio com reflexo" (095/T01, `--chrome`, `--chrome-muted`, `--hi`, `--lo`,
 *   `--bevel`): base opaca de 3 paradas com faixa de luz, ponto de brilho e faixa diagonal por cima,
 *   na barra superior, nas barras de título e nos segmentados; as sobreposições são `--overlay`;
 * - a estrutura interna das janelas sem borda (095/T01): `--divider`, `--lane` e `--card-edge`;
 * - a escala de acabamento (094/T06): raios `--r-*`, espaços `--sp-*`, fontes `--fs-*`/`--fw-*`,
 *   elevação `--elev-1..4`, estados `--state-*` e os tamanhos de ícone;
 * - as pílulas de fase, o tema do CodeMirror e o favicon de atenção.
 *
 * As cores são aplicadas pelo CSSOM (`style.setProperty`/`style` do React), o que a CSP
 * `style-src 'self'` permite; nada injeta `<style>`.
 */
import type { ProjectPhase } from "../../store";
import { localized } from "../../i18n";

export type ThemeName = "dark" | "light";
export type ThemePreference = ThemeName | "auto";
export type WallpaperId = "dots" | "nebula" | "aurora" | "plain";

export const THEME_NAMES: readonly ThemeName[] = ["dark", "light"];
export const THEME_PREFERENCES: readonly ThemePreference[] = ["dark", "light", "auto"];
export const WALLPAPER_IDS: readonly WallpaperId[] = ["dots", "nebula", "aurora", "plain"];

export const THEME_LABELS: Readonly<Record<ThemePreference, string>> = localized({
  dark: "appearance.theme.dark",
  light: "appearance.theme.light",
  auto: "appearance.theme.auto",
});

export const WALLPAPER_LABELS: Readonly<Record<WallpaperId, string>> = localized({
  dots: "appearance.wallpaper.dots",
  nebula: "appearance.wallpaper.nebula",
  aurora: "appearance.wallpaper.aurora",
  plain: "appearance.wallpaper.plain",
});

/** Cores do símbolo da marca (iguais nos dois temas). */
export const BRAND = {
  violet: "#A78BFA",
  cyan: "#22D3EE",
  amber: "#FBBF24",
} as const;

/**
 * Cores do Nanty (spec 138 T04, `reference/Avatar.dc.html`): a cabeça e as antenas nas três cores
 * da marca, o traço escuro dos olhos e da boca, o brilho dos olhos, o "z" do sono e o ponto âmbar
 * de "esperando você". Iguais nos dois temas, como o símbolo; `photo` é o círculo da foto do bot.
 */
export const NANTY = {
  head: BRAND.violet,
  antennaLeft: BRAND.cyan,
  antennaRight: BRAND.amber,
  feature: "#1B1530",
  shine: "#FFFFFF",
  sleep: "#AEAEB8",
  waiting: "#F59E0B",
  photo: "#1B1530",
} as const;

/** Variantes escuras da marca: ícones, pílulas e chips no tema claro (decisão 13). */
export const BRAND_DARK = {
  cyan: "#0E7490",
  violet: "#6D28D9",
  amber: "#92400E",
} as const;

/**
 * Tinta de perigo por tema (094/T08, A-15): o mesmo tom em `--danger-ink` (erros, ações
 * destrutivas) e na pílula rosa (`--pill-rose-ink`), para os tons de perigo não divergirem.
 */
export const DANGER_INK = {
  // Rosa-200: ≥ 4,5:1 sobre a pílula no titânio da barra (o rosa-300 ficava abaixo).
  dark: "#FECDD3",
  // Rosa-800: ≥ 4,5:1 também sob a camada de hover/pressionado (094/T06).
  light: "#9F1239",
} as const;

/*
 * Titânio com reflexo (095/T01, decisão 1; variante "Titânio com reflexo" de
 * `reference/Cromo.dc.html`). Só gradientes, sem textura nem vidro. De baixo para cima:
 * 1. base opaca com 3 paradas (topo, meio, base);
 * 2. faixa diagonal de reflexo (pico e cauda);
 * 3. ponto de brilho radial no topo;
 * 4. faixa de luz superior (alfa máximo no topo, some no meio) com a sombra na base.
 * Por cima vai a borda de luz no topo (`inset 0 1px 0`, `--hi`) e a linha da base (`--lo`).
 *
 * Os valores são os do mockup e o reflexo nunca fica abaixo dele (decisão do Arquiteto na revisão
 * da T01). O contraste (C-01) é medido onde o conteúdo fica: `titaniumExtremes` (em `contrast.ts`)
 * amostra uma grade x × y com o alfa real de cada camada em cada ponto, na área de conteúdo das
 * superfícies de titânio (`CHROME_SURFACES`): de 25% a 75% da altura nas barras e nos títulos,
 * onde os controles são centralizados. Os testes exigem 4,5:1 (texto) e 3:1 (ícone e foco)
 * na amostra mais clara e na mais escura. O que não fecha se resolve nas cores do conteúdo sobre o
 * cromo (`chromeInk`, `chromeMuted`, `chromeIcon`, `chromePillInk`), nunca no reflexo.
 *
 * O titânio fica SÓ na barra superior, nas barras de título (e nas abas do celular) e nos
 * controles segmentados. As sobreposições (menus, popovers, lançador, avisos, dicas) são uma
 * superfície sólida elevada (`overlay`), como no mockup (decisão do Arquiteto na revisão 1 da T01).
 */
interface Titanium {
  /** Base opaca: topo, meio (em `midAt`) e base. */
  base: { top: string; mid: string; midAt: string; bottom: string };
  /** Faixa de luz superior: `top` no 0%, `mid` em `midAt`, transparente em `clearAt`, `shade` no 100%. */
  band: { top: string; mid: string; midAt: string; clearAt: string; shade: string };
  /** Ponto de brilho radial (alfa máximo no centro). */
  glint: string;
  /** Faixa diagonal de reflexo: pico e cauda. */
  streak: { peak: string; tail: string };
  /** Borda de luz no topo (`--hi`) e a linha escura da base (`--lo`), ambas `inset` de 1 px. */
  edgeTop: string;
  edgeBottom: string;
}

/** Geometria do reflexo (igual nos dois temas; as cores mudam), a do mockup. */
export const TITANIUM_GEOMETRY = {
  glint: { size: "45% 260%", at: "66% 0%", fade: "65%" },
  streak: { angle: 100, from: "30%", peak: "44%", tail: "50%", to: "60%" },
} as const;

/** CSS do titânio: as camadas de cima para baixo (a ordem do `background`). */
export function titaniumCss(ti: Titanium): string {
  const g = TITANIUM_GEOMETRY;
  return [
    `linear-gradient(180deg, ${ti.band.top} 0%, ${ti.band.mid} ${ti.band.midAt}, transparent ${ti.band.clearAt}, ${ti.band.shade} 100%)`,
    `radial-gradient(${g.glint.size} at ${g.glint.at}, ${ti.glint}, transparent ${g.glint.fade})`,
    `linear-gradient(${g.streak.angle}deg, transparent ${g.streak.from}, ${ti.streak.peak} ${g.streak.peak}, ${ti.streak.tail} ${g.streak.tail}, transparent ${g.streak.to})`,
    `linear-gradient(180deg, ${ti.base.top} 0%, ${ti.base.mid} ${ti.base.midAt}, ${ti.base.bottom} 100%)`,
  ].join(", ");
}

/** Tons com que ícones, pílulas e chips são pintados. */
export type Tone = "violet" | "cyan" | "amber" | "green" | "rose" | "muted";
export const TONES: readonly Tone[] = ["violet", "cyan", "amber", "green", "rose", "muted"];

// --- Escala de acabamento (spec 094, T06) ---
/*
 * Uma escala única para raios, espaços, elevação, tipografia e ícones. Todo CSS de `desktop/**` e
 * `apps/**` usa só estes tokens em `border-radius`, `box-shadow`, `font-size`/`font`, `gap` e
 * `padding` (checagem automática em `checks.test.ts`, com as exceções justificadas lá).
 */

/**
 * Raios (px). Raios aninhados são concêntricos: o interno é o externo menos o padding entre eles
 * (menu `lg` − `sp-1` = item `md`; lançador `xl` − `sp-2` da lista =
 * resultado `md`; pílula `lg` − `sp-1` = Parar `md`). O `xl` (16) é o raio do lançador (095/T02).
 */
export const RADII = { xs: 4, sm: 6, md: 8, lg: 12, xl: 16, "2xl": 20, pill: 999 } as const;

/** Espaçamentos (px), sempre múltiplos de 4: `--sp-<n>` vale `n × 4`. */
export const SPACE = { 1: 4, 2: 8, 3: 12, 4: 16, 5: 20, 6: 24, 8: 32, 10: 40, 12: 48 } as const;

/**
 * Escala tipográfica (095/T02, C-09), a do mockup (`reference/*.dc.html`): um tamanho por papel.
 * O corpo do app é 14 px; os controles (título de janela, botão, pílula, linha densa) ficam em 13.
 */
export const FONT_SIZES = {
  /** Selos e contadores mínimos (selo dos apps, pílula pequena). */
  micro: "10px",
  /** Legenda: hora, rótulo em caixa alta e `<kbd>`. */
  caption: "11px",
  /** Rótulo: metadados, subtítulo da janela e texto secundário. */
  label: "12px",
  /** Controle: título da janela (com `--fw-semibold`), botão, pílula e linha densa. */
  control: "13px",
  /** Corpo de texto (mensagens, listas, campos). */
  body: "14px",
  /** Títulos de seção, diálogo e o campo do lançador. */
  heading: "17px",
  /** Título de destaque (Plano). */
  display: "20px",
  /** Números grandes (Custos). */
  numeral: "32px",
  /** Código, console, ids de tarefa, caminhos e hashes (monoespaçada; lista fechada, C-08). */
  mono: "12px",
  /** Campos no celular: abaixo de 16 px o Safari do iPhone amplia a página ao focar. */
  touch: "16px",
  /** Código em linha dentro de texto corrido. */
  "code-inline": "0.88em",
} as const;

/** Pesos da fonte (C-15: o 700 saiu do uso; o mais forte é o 600 da Inter importada). */
export const FONT_WEIGHTS = { regular: 400, medium: 500, semibold: 600 } as const;

/** Entrelinha do corpo e espaçamento entre letras do mockup (`letter-spacing: -0.006em`), na `.nd-root`. */
export const LINE_HEIGHT_BODY = 1.55;
export const LETTER_SPACING_TIGHT = "-0.006em";

/** Tamanhos de ícone (px) e o traço único do conjunto (`Icon`). */
export const ICON_SIZES = [14, 16, 18, 20] as const;
export type IconSize = (typeof ICON_SIZES)[number];
export const ICON_STROKE = 1.8;

/** Níveis de elevação (sombra) e o papel de cada um. */
type Elevation = "raised" | "back" | "menu" | "front";
export const ELEVATIONS: readonly Elevation[] = ["raised", "back", "menu", "front"];
/** `--elev-1` superfície elevada baixa (era a do dock, que saiu na 137/T02), `--elev-2` janela atrás, `--elev-3` menu/popover, `--elev-4` janela em foco e diálogo. */
export const ELEVATION_LEVEL: Readonly<Record<Elevation, number>> = { raised: 1, back: 2, menu: 3, front: 4 };

/**
 * Bisel do titânio: a borda de luz no topo (`--hi`) e a linha escura na base (`--lo`), que separa a
 * barra de título do conteúdo sem `border`. Igual nos dois temas (as cores mudam).
 */
export const BEVEL = "inset 0 1px 0 var(--hi), inset 0 -1px 0 var(--lo)";

/**
 * Alfa mínimo do véu (`--scrim`) atrás das sobreposições modais, por tema (094/T08, V-01). O painel
 * modal em si é sólido (`--surface2`), sem nada da janela de trás através dele.
 */
export const SCRIM_MIN_ALPHA: Readonly<Record<"dark" | "light", number>> = { dark: 0.55, light: 0.3 };

/**
 * Separação mínima (razão de contraste WCAG) entre a janela branca (`--surface`) e a cor mais clara
 * de cada papel de parede do tema claro (095/T01, C-13). É a separação do `#EEEEF4`, o teto pedido
 * pela consultoria para a base dos papéis claros (1,156), arredondada para baixo.
 */
export const LIGHT_WINDOW_SEPARATION = 1.15;

/** Opacidade do controle desabilitado (o mesmo em todos os controles). */
export const DISABLED_OPACITY = 0.6;

/**
 * QR code do endereço de entrada (spec 112): módulos pretos sobre fundo branco nos dois temas, o
 * contraste que os leitores de QR esperam (invertido, muitos não leem).
 */
export const QR_COLORS = { light: "#ffffff", dark: "#000000" } as const;

/** Anel de seleção (item escolhido numa grade de opções): fino, na cor de seleção (≥ 3:1). */
export const SELECTED_RING = "0 0 0 1px var(--selected-edge)";

/** Destaque da pílula ao integrar: anel na cor da fase concluída. */
export const FLASH_RING = "0 0 0 2px var(--pill-green-edge)";

/** Variáveis CSS da escala (independentes do tema). */
export function scaleVariables(): Record<`--${string}`, string> {
  const vars: Record<`--${string}`, string> = {};
  for (const [name, px] of Object.entries(RADII)) vars[`--r-${name}`] = `${px}px`;
  for (const [step, px] of Object.entries(SPACE)) vars[`--sp-${step}`] = `${px}px`;
  for (const [role, size] of Object.entries(FONT_SIZES)) vars[`--fs-${role}`] = size;
  for (const [name, weight] of Object.entries(FONT_WEIGHTS)) vars[`--fw-${name}`] = String(weight);
  vars["--lh-body"] = String(LINE_HEIGHT_BODY);
  vars["--ls-tight"] = LETTER_SPACING_TIGHT;
  vars["--o-disabled"] = String(DISABLED_OPACITY);
  vars["--bevel"] = BEVEL;
  vars["--ring-selected"] = SELECTED_RING;
  vars["--ring-flash"] = FLASH_RING;
  return vars;
}

/** Pílula (fase, execução, chip): texto, fundo e borda. */
interface PillTokens {
  ink: string;
  bg: string;
  edge: string;
}

interface ThemeTokens {
  // Superfícies opacas (o fundo das janelas é sempre sólido).
  bg: string;
  surface: string;
  surface2: string;
  card: string;
  // Texto.
  ink: string;
  ink2: string;
  ink3: string;
  muted: string;
  // Linhas e divisores (decorativos; as bordas que identificam algo têm token próprio).
  line: string;
  line2: string;
  /** Linha fina entre seções e itens dentro das janelas (095/T01, `Main.dc.html`). */
  divider: string;
  /** Fundo levemente diferente para raias e grupos (095/T01). */
  lane: string;
  /** Relevo sutil de cartão, sem contorno (095/T01): uma sombra, em `FAMILY_VARS.shadow`. */
  cardEdge: string;
  /** Véu atrás das sobreposições modais (lançador, diálogos): escurece a janela de trás. */
  scrim: string;
  shadow: string;
  /** Material titânio com reflexo (barra superior, títulos e segmentados). */
  titanium: Titanium;
  /**
   * Menu do N (spec 137, `reference/menu-n.dc.html`): o botão em relevo discreto na barra (brilho em
   * cima e sombra curta embaixo) que afunda com o menu aberto (sombra interna e fundo mais escuro,
   * sem contorno colorido), o ponto rosa de novidade com o anel na cor do titânio, a sombra do
   * menu (um degrau acima das sobreposições) e o véu que escurece a área de trabalho.
   */
  nMenu: {
    buttonBg: string;
    buttonBgOpen: string;
    buttonShadow: string;
    buttonShadowOpen: string;
    dot: string;
    dotRing: string;
    shadow: string;
    scrim: string;
  };
  /**
   * Superfície sólida elevada das sobreposições (menus, popovers, sino, folha de atalhos,
   * Personalizar, lançador, avisos, toasts e dicas; classe `.nd-overlay`), como no mockup
   * (`Janelas.dc.html`: busca `#16151F`; `App.dc.html`: aviso `#1E1C29`). Dentro dela valem os
   * tokens normais (`--ink`, `--muted`, `--icon-*`, `--state-*`).
   */
  overlay: string;
  /** Borda de luz no topo da sobreposição (`inset 0 1px 0`), com a elevação `--elev-3` por fora. */
  overlayEdge: string;
  /*
   * Cores do conteúdo SOBRE o titânio (C-01; decisão do Arquiteto na revisão da T01: o que não fecha
   * contraste onde o conteúdo fica se resolve aqui, nunca reduzindo o reflexo). Dentro de
   * `.nd-chrome` elas substituem `--ink`/`--ink2`/`--ink3`, `--muted`, `--control-edge`,
   * `--icon-*` e `--selected-edge` (regra em `desktop.css`).
   */
  /** Texto principal sobre o titânio (inclui os estados hover e pressionado). */
  chromeInk: string;
  /** Texto secundário, borda de controle e ícone neutro sobre o titânio. */
  chromeMuted: string;
  /** Ícones por tom sobre o titânio (barras), também em hover, pressionado e foco. */
  chromeIcon: Readonly<Record<Tone, string>>;
  /** Texto das pílulas sobre o titânio (pílula de execução); o rosa também é o `--danger-ink` lá. */
  chromePillInk: Readonly<Record<Tone, string>>;
  // Bordas que identificam componentes (≥ 3:1).
  edgeStrong: string;
  /**
   * Indicador de seleção (item escolhido num grupo, app em foco): ≥ 3:1. Nunca é a
   * moldura da janela, que não tem borda (095/T01): a sombra e o contraste da superfície a separam.
   */
  selectedEdge: string;
  controlEdge: string;
  /**
   * Fundo neutro de controle (095/T02, decisão 9): botão secundário, pílulas de estado, seletor de
   * projeto, busca e interruptores da barra. A cor fica só no ponto e no rótulo, nunca no fundo.
   */
  control: string;
  /**
   * O mesmo fundo neutro sobre o titânio (dentro de `.nd-chrome` vale como `--control`). No claro
   * ele clareia o metal (o escurecido do mockup deixava o rótulo ciano da pílula abaixo de 4,5:1).
   */
  chromeControl: string;
  // Estados dos controles: camadas translúcidas por cima de qualquer fundo (094/T06).
  stateHover: string;
  statePress: string;
  /** Elevação (sombras) por nível, calibrada por tema: no claro, mais suave. */
  elevation: Readonly<Record<Elevation, string>>;
  // Foco duplo (claro + escuro).
  focusInner: string;
  focusOuter: string;
  // Ação principal.
  accent: string;
  accentInk: string;
  accentSoft: string;
  // Ícones por tom.
  icon: Readonly<Record<Tone, string>>;
  // Selo dos apps (menu do N e celular).
  badgeBg: string;
  badgeInk: string;
  // Prévia de encaixe.
  snapFill: string;
  snapEdge: string;
  // Botão fechar (hover) e ações destrutivas.
  dangerSoft: string;
  dangerInk: string;
  // Pílulas por tom (fase, execução, chips).
  pill: Readonly<Record<Tone, PillTokens>>;
}

export const THEMES: Readonly<Record<ThemeName, ThemeTokens>> = {
  dark: {
    bg: "#050508",
    surface: "#0D0D15",
    surface2: "#11111B",
    card: "#151522",
    ink: "#F4F4F6",
    ink2: "#E4E4E7",
    ink3: "#D4D4D8",
    // Um pouco mais claro que o do mockup: ≥ 4,5:1 nas superfícies das janelas.
    muted: "#AEAEB8",
    line: "rgba(255, 255, 255, 0.07)",
    line2: "rgba(255, 255, 255, 0.11)",
    divider: "rgba(255, 255, 255, 0.07)",
    lane: "rgba(255, 255, 255, 0.025)",
    cardEdge: "inset 0 1px 0 rgba(255, 255, 255, 0.05), 0 1px 2px rgba(0, 0, 0, 0.35)",
    scrim: "rgba(0, 0, 0, 0.62)",
    shadow: "rgba(0, 0, 0, 0.55)",
    titanium: {
      /*
       * Valores do mockup, sem desvio (decisão do Arquiteto na revisão da T01): o reflexo nunca fica
       * abaixo do mockup. Uma 1ª versão reduziu os alfas a ~40% porque o modelo somava todas as
       * camadas no topo (y = 0), onde não há conteúdo, e o metal ficou chapado. O contraste é medido
       * na área de conteúdo de cada superfície (`CHROME_SURFACES`, em `contrast.ts`); o que não fecha lá é
       * resolvido nas cores do conteúdo sobre o cromo (`chromeIcon`, `chromeMuted`), nunca no reflexo.
       */
      base: { top: "#3A3A46", mid: "#2B2B35", midAt: "52%", bottom: "#212129" },
      band: { top: "rgba(255, 255, 255, 0.16)", mid: "rgba(255, 255, 255, 0.06)", midAt: "38%", clearAt: "52%", shade: "rgba(0, 0, 0, 0.1)" },
      glint: "rgba(255, 255, 255, 0.14)",
      streak: { peak: "rgba(255, 255, 255, 0.1)", tail: "rgba(255, 255, 255, 0.02)" },
      edgeTop: "rgba(255, 255, 255, 0.16)",
      edgeBottom: "rgba(0, 0, 0, 0.45)",
    },
    nMenu: {
      buttonBg: "linear-gradient(180deg, #34343F, #1F1F28)",
      buttonBgOpen: "linear-gradient(180deg, #121219, #1A1A23)",
      buttonShadow:
        "inset 0 1px 0 rgba(255, 255, 255, 0.14), inset 0 -2px 0 rgba(0, 0, 0, 0.35), 0 3px 8px rgba(0, 0, 0, 0.45), 0 1px 2px rgba(0, 0, 0, 0.4), 0 0 0 1px rgba(255, 255, 255, 0.06)",
      buttonShadowOpen: "inset 0 3px 8px rgba(0, 0, 0, 0.7), inset 0 1px 2px rgba(0, 0, 0, 0.6), 0 1px 0 rgba(255, 255, 255, 0.06)",
      dot: "#F43F5E",
      // A base do titânio no meio da barra: o anel separa o ponto do botão.
      dotRing: "0 0 0 2px #2B2B35",
      shadow: "0 28px 70px rgba(0, 0, 0, 0.6), 0 0 0 1px rgba(255, 255, 255, 0.07), inset 0 1px 0 rgba(255, 255, 255, 0.06)",
      scrim: "rgba(0, 0, 0, 0.55)",
    },
    /*
     * No ponto mais claro das barras (faixa de 25% a 75%, L≈0,12) o `--ink` (#F4F4F6) cai para 4,2:1 com a
     * camada de pressionado, o `--muted` para 2,4:1 e os ícones violeta e rosa para 2,2:1. Sobre o
     * titânio o texto é branco, o secundário mais claro e os ícones usam os tons claros do mockup
     * (`Main.dc.html`: #C4B5FD/#67E8F9/#86EFAC/#FCD34D), um passo acima quando preciso.
     */
    chromeInk: "#FFFFFF",
    chromeMuted: "#DEDEE6",
    chromeIcon: {
      violet: "#DDD6FE",
      cyan: "#67E8F9",
      amber: "#FCD34D",
      green: "#86EFAC",
      rose: "#FECDD3",
      muted: "#DEDEE6",
    },
    // As tintas -200 das pílulas ficam em 4,1–4,4:1 sobre o fundo da pílula no titânio: um passo acima (-100).
    chromePillInk: {
      violet: "#EDE9FE",
      cyan: "#CFFAFE",
      amber: "#FEF3C7",
      green: "#DCFCE7",
      rose: "#FFE4E6",
      muted: "#F4F4F5",
    },
    // Um nível só, entre a busca (#16151F) e o aviso (#1E1C29) do mockup: acima de `--surface2`.
    overlay: "#1A1924",
    overlayEdge: "inset 0 1px 0 rgba(255, 255, 255, 0.08)",
    edgeStrong: "#8B8BA7",
    // Neutro (095/T07, pedido do usuário): seleção e foco nunca desenham contorno violeta.
    selectedEdge: "#D4D4D8",
    // ≥ 3:1 nas janelas; sobre o titânio a borda de controle é o `--chrome-muted` (C-01).
    controlEdge: "#8E8EA6",
    control: "rgba(255, 255, 255, 0.07)",
    chromeControl: "rgba(0, 0, 0, 0.22)",
    stateHover: "rgba(255, 255, 255, 0.07)",
    statePress: "rgba(255, 255, 255, 0.12)",
    /*
     * Janelas sem borda (095/T01): dois níveis de sombra nas janelas, `back` (atrás) e `front` (em
     * foco, a `winShadow` do mockup). Menus mantêm o nível 3 (C-16).
     */
    elevation: {
      raised: "0 8px 24px rgba(0, 0, 0, 0.36), 0 1px 2px rgba(0, 0, 0, 0.4)",
      back: "0 20px 36px -16px rgba(0, 0, 0, 0.7), 0 4px 12px rgba(0, 0, 0, 0.38)",
      menu: "0 16px 40px rgba(0, 0, 0, 0.5), 0 2px 8px rgba(0, 0, 0, 0.32)",
      front: "0 36px 80px -24px rgba(0, 0, 0, 0.85), 0 10px 28px rgba(0, 0, 0, 0.45)",
    },
    focusInner: "#050508",
    // Anel de foco neutro e discreto (spec 095, decisão 2): claro sobre o escuro, nunca violeta.
    focusOuter: "#E4E4E7",
    accent: "#7C3AED",
    accentInk: "#FFFFFF",
    accentSoft: "rgba(167, 139, 250, 0.16)",
    icon: {
      violet: "#A78BFA",
      cyan: "#22D3EE",
      amber: "#FBBF24",
      green: "#4ADE80",
      rose: "#FB7185",
      muted: "#A1A1AA",
    },
    badgeBg: "#FBBF24",
    badgeInk: "#1C1917",
    snapFill: "rgba(167, 139, 250, 0.14)",
    snapEdge: "rgba(167, 139, 250, 0.6)",
    dangerSoft: "rgba(244, 63, 94, 0.18)",
    dangerInk: DANGER_INK.dark,
    pill: {
      // Tons claros o bastante para ≥ 4,5:1 sobre o fundo da pílula composto no titânio.
      violet: { ink: "#DDD6FE", bg: "rgba(167, 139, 250, 0.08)", edge: "rgba(167, 139, 250, 0.3)" },
      cyan: { ink: "#A5F3FC", bg: "rgba(34, 211, 238, 0.08)", edge: "rgba(34, 211, 238, 0.25)" },
      amber: { ink: "#FDE68A", bg: "rgba(251, 191, 36, 0.08)", edge: "rgba(251, 191, 36, 0.3)" },
      green: { ink: "#BBF7D0", bg: "rgba(74, 222, 128, 0.08)", edge: "rgba(74, 222, 128, 0.25)" },
      rose: { ink: DANGER_INK.dark, bg: "rgba(244, 63, 94, 0.12)", edge: "rgba(253, 164, 175, 0.35)" },
      muted: { ink: "#E4E4E7", bg: "rgba(255, 255, 255, 0.04)", edge: "rgba(255, 255, 255, 0.11)" },
    },
  },
  light: {
    bg: "#EEEEF4",
    surface: "#FFFFFF",
    surface2: "#F6F6FA",
    card: "#F7F7FB",
    ink: "#16161F",
    ink2: "#26262F",
    ink3: "#3F3F46",
    muted: "#5B5B66",
    line: "rgba(17, 17, 27, 0.08)",
    line2: "rgba(17, 17, 27, 0.13)",
    divider: "rgba(17, 17, 27, 0.08)",
    lane: "rgba(17, 17, 27, 0.025)",
    cardEdge: "0 1px 3px rgba(17, 17, 27, 0.08)",
    scrim: "rgba(17, 17, 27, 0.38)",
    shadow: "rgba(30, 30, 60, 0.18)",
    titanium: {
      // Valores do mockup, sem desvio: onde há conteúdo o contraste fecha com eles.
      base: { top: "#EDEDF2", mid: "#DEDEE5", midAt: "52%", bottom: "#D2D2DA" },
      band: { top: "rgba(255, 255, 255, 0.95)", mid: "rgba(255, 255, 255, 0.5)", midAt: "38%", clearAt: "52%", shade: "rgba(17, 17, 27, 0.05)" },
      glint: "rgba(255, 255, 255, 0.9)",
      streak: { peak: "rgba(255, 255, 255, 0.8)", tail: "rgba(255, 255, 255, 0.1)" },
      edgeTop: "rgba(255, 255, 255, 0.95)",
      edgeBottom: "rgba(17, 17, 27, 0.07)",
    },
    nMenu: {
      buttonBg: "linear-gradient(180deg, #FFFFFF, #ECECF2)",
      buttonBgOpen: "linear-gradient(180deg, #E2E2EA, #ECECF2)",
      buttonShadow:
        "inset 0 1px 0 #FFFFFF, inset 0 -2px 0 rgba(17, 17, 27, 0.08), 0 3px 8px rgba(17, 17, 27, 0.14), 0 1px 2px rgba(17, 17, 27, 0.1), 0 0 0 1px rgba(17, 17, 27, 0.06)",
      buttonShadowOpen: "inset 0 3px 8px rgba(17, 17, 27, 0.22), inset 0 1px 2px rgba(17, 17, 27, 0.18), 0 1px 0 rgba(255, 255, 255, 0.9)",
      dot: "#F43F5E",
      dotRing: "0 0 0 2px #DEDEE5",
      shadow: "0 28px 70px rgba(17, 17, 27, 0.28), 0 0 0 1px rgba(17, 17, 27, 0.06)",
      scrim: "rgba(17, 17, 27, 0.28)",
    },
    // No ponto mais escuro (L≈0,66) o `--muted` (#5B5B66) fica em 3,8:1 com o pressionado e o verde
    // em 2,8:1; o ciano, em 3,0:1: sobre o titânio escurecem um passo.
    chromeInk: "#16161F",
    chromeMuted: "#4A4A55",
    chromeIcon: {
      violet: BRAND_DARK.violet,
      cyan: "#155E75",
      amber: BRAND_DARK.amber,
      green: "#166534",
      rose: "#BE123C",
      muted: "#4A4A55",
    },
    // As pílulas claras são quase opacas: as tintas das janelas já fecham sobre o titânio.
    chromePillInk: {
      violet: BRAND_DARK.violet,
      cyan: BRAND_DARK.cyan,
      amber: BRAND_DARK.amber,
      green: "#166534",
      rose: DANGER_INK.light,
      muted: "#3F3F46",
    },
    // Branco, como a janela: a elevação (`--elev-3`) separa a sobreposição do que está atrás.
    overlay: "#FFFFFF",
    overlayEdge: "inset 0 1px 0 rgba(255, 255, 255, 0.9)",
    edgeStrong: "#6E6E80",
    selectedEdge: "#3F3F46",
    controlEdge: "#7E7E8E",
    control: "rgba(17, 17, 27, 0.04)",
    chromeControl: "rgba(255, 255, 255, 0.6)",
    stateHover: "rgba(17, 17, 27, 0.05)",
    statePress: "rgba(17, 17, 27, 0.09)",
    elevation: {
      raised: "0 8px 24px rgba(30, 30, 60, 0.12), 0 1px 2px rgba(30, 30, 60, 0.1)",
      back: "0 20px 36px -16px rgba(40, 28, 80, 0.24), 0 4px 12px rgba(40, 28, 80, 0.08)",
      menu: "0 16px 40px rgba(30, 30, 60, 0.16), 0 2px 8px rgba(30, 30, 60, 0.08)",
      front: "0 30px 70px -22px rgba(40, 28, 80, 0.32), 0 6px 18px rgba(40, 28, 80, 0.1)",
    },
    focusInner: "#FFFFFF",
    focusOuter: "#3F3F46",
    accent: BRAND_DARK.violet,
    accentInk: "#FFFFFF",
    accentSoft: "rgba(109, 40, 217, 0.1)",
    icon: {
      violet: BRAND_DARK.violet,
      cyan: BRAND_DARK.cyan,
      amber: BRAND_DARK.amber,
      green: "#15803D",
      rose: "#BE123C",
      muted: "#5B5B66",
    },
    badgeBg: BRAND_DARK.amber,
    badgeInk: "#FFFFFF",
    snapFill: "rgba(109, 40, 217, 0.12)",
    snapEdge: "rgba(109, 40, 217, 0.55)",
    dangerSoft: "rgba(190, 18, 60, 0.1)",
    dangerInk: DANGER_INK.light,
    pill: {
      // Fundos claros quase opacos: a variante escura da marca fica ≥ 4,5:1 sobre qualquer papel de parede.
      violet: { ink: BRAND_DARK.violet, bg: "rgba(245, 243, 255, 0.92)", edge: "rgba(109, 40, 217, 0.3)" },
      cyan: { ink: BRAND_DARK.cyan, bg: "rgba(236, 254, 255, 0.92)", edge: "rgba(14, 116, 144, 0.32)" },
      amber: { ink: BRAND_DARK.amber, bg: "rgba(255, 251, 235, 0.92)", edge: "rgba(146, 64, 14, 0.3)" },
      green: { ink: "#166534", bg: "rgba(240, 253, 244, 0.92)", edge: "rgba(21, 128, 61, 0.3)" },
      rose: { ink: DANGER_INK.light, bg: "rgba(255, 241, 242, 0.92)", edge: "rgba(190, 18, 60, 0.3)" },
      muted: { ink: "#3F3F46", bg: "rgba(250, 250, 252, 0.92)", edge: "rgba(17, 17, 27, 0.13)" },
    },
  },
};

/** Tom da pílula de cada fase do projeto. */
export const PHASE_TONE: Readonly<Record<ProjectPhase, Tone>> = {
  idle: "muted",
  planning: "violet",
  reviewing_plan: "violet",
  waiting_approval: "amber",
  executing: "cyan",
  finalizing: "cyan",
  integrated: "green",
  blocked: "rose",
  stalled: "amber",
};

/** Pílula de execução (barra superior): o tom ciano de "Executando". */
export const RUN_PILL_TONE: Tone = "cyan";

// --- Papéis de parede ---

/** Camada de um papel de parede, de baixo para cima por cima da cor base. */
type WallpaperLayer =
  | { kind: "dots"; color: string; spacing: number }
  | { kind: "radial"; color: string; at: string; size: string }
  | { kind: "linear"; angle: number; stops: readonly string[] };

interface WallpaperTheme {
  base: string;
  layers: readonly WallpaperLayer[];
}

export const WALLPAPERS: Readonly<Record<WallpaperId, Readonly<Record<ThemeName, WallpaperTheme>>>> = {
  dots: {
    dark: { base: "#050508", layers: [{ kind: "dots", color: "rgba(167, 139, 250, 0.1)", spacing: 28 }] },
    light: { base: "#EEEEF4", layers: [{ kind: "dots", color: "rgba(109, 40, 217, 0.1)", spacing: 28 }] },
  },
  nebula: {
    dark: {
      base: "#07071A",
      layers: [
        { kind: "radial", color: "rgba(167, 139, 250, 0.3)", at: "78% 30%", size: "60% 50%" },
        { kind: "radial", color: "rgba(34, 211, 238, 0.18)", at: "20% 80%", size: "45% 45%" },
        { kind: "radial", color: "rgba(236, 72, 153, 0.14)", at: "55% 55%", size: "40% 35%" },
      ],
    },
    light: {
      // 095/T01 (C-13): base mais escura (era #F1EEFB) para a janela branca se separar do papel.
      base: "#EDEAF8",
      layers: [
        { kind: "radial", color: "rgba(167, 139, 250, 0.35)", at: "78% 30%", size: "60% 50%" },
        { kind: "radial", color: "rgba(34, 211, 238, 0.25)", at: "20% 80%", size: "45% 45%" },
      ],
    },
  },
  aurora: {
    dark: {
      base: "#050508",
      layers: [{ kind: "linear", angle: 160, stops: ["#050508", "#0B1B2B", "#0E3A3F", "#1B1440"] }],
    },
    light: {
      // 095/T01 (C-13): paradas mais escuras (eram #F5F7FF e #E3F4F7) para a janela branca se separar.
      base: "#EBEEF8",
      layers: [{ kind: "linear", angle: 160, stops: ["#EBEEF8", "#E1F1F5", "#EFE7FF"] }],
    },
  },
  plain: {
    dark: { base: "#08080F", layers: [] },
    light: { base: "#EEEEF4", layers: [] },
  },
};

const TRANSPARENT = "transparent";

function layerImage(layer: WallpaperLayer): string {
  switch (layer.kind) {
    case "dots":
      return `radial-gradient(${layer.color} 1px, ${TRANSPARENT} 1px)`;
    case "radial":
      return `radial-gradient(${layer.size} at ${layer.at}, ${layer.color}, ${TRANSPARENT} 70%)`;
    case "linear": {
      const last = Math.max(1, layer.stops.length - 1);
      const stops = layer.stops.map((color, i) => `${color} ${Math.round((i / last) * 100)}%`).join(", ");
      return `linear-gradient(${layer.angle}deg, ${stops})`;
    }
  }
}

/** Estilo (CSSOM) do papel de parede: cor base + camadas. */
export function wallpaperStyle(id: WallpaperId, theme: ThemeName): {
  backgroundColor: string;
  backgroundImage?: string;
  backgroundSize?: string;
} {
  const wall = WALLPAPERS[id][theme];
  if (wall.layers.length === 0) return { backgroundColor: wall.base };
  // A primeira camada fica por cima (ordem do CSS): invertemos a lista "de baixo para cima".
  const ordered = [...wall.layers].reverse();
  const images = ordered.map(layerImage).join(", ");
  const sizes = ordered.map((l) => (l.kind === "dots" ? `${l.spacing}px ${l.spacing}px` : "100% 100%")).join(", ");
  return { backgroundColor: wall.base, backgroundImage: images, backgroundSize: sizes };
}

// --- Variáveis CSS ---

type CssVariables = Record<`--${string}`, string>;

/** Variáveis CSS de um tema (aplicadas na raiz do desktop). */
export function themeVariables(theme: ThemeName): CssVariables {
  const t = THEMES[theme];
  const vars: CssVariables = {
    "--bg": t.bg,
    "--surface": t.surface,
    "--surface2": t.surface2,
    "--card": t.card,
    "--ink": t.ink,
    "--ink2": t.ink2,
    "--ink3": t.ink3,
    "--muted": t.muted,
    "--line": t.line,
    "--line2": t.line2,
    "--divider": t.divider,
    "--lane": t.lane,
    "--card-edge": t.cardEdge,
    "--scrim": t.scrim,
    "--shadow": t.shadow,
    "--chrome": titaniumCss(t.titanium),
    "--overlay": t.overlay,
    "--nbtn-bg": t.nMenu.buttonBg,
    "--nbtn-bg-open": t.nMenu.buttonBgOpen,
    "--elev-nbtn": t.nMenu.buttonShadow,
    "--elev-nbtn-open": t.nMenu.buttonShadowOpen,
    "--ndot": t.nMenu.dot,
    "--ring-ndot": t.nMenu.dotRing,
    "--elev-nmenu": t.nMenu.shadow,
    "--nmenu-scrim": t.nMenu.scrim,
    "--overlay-edge": t.overlayEdge,
    "--chrome-ink": t.chromeInk,
    "--chrome-muted": t.chromeMuted,
    "--hi": t.titanium.edgeTop,
    "--lo": t.titanium.edgeBottom,
    "--edge-strong": t.edgeStrong,
    "--selected-edge": t.selectedEdge,
    "--control-edge": t.controlEdge,
    "--control": t.control,
    "--chrome-control": t.chromeControl,
    "--state-hover": t.stateHover,
    "--state-press": t.statePress,
    "--focus-inner": t.focusInner,
    "--focus-outer": t.focusOuter,
    "--accent": t.accent,
    "--accent-ink": t.accentInk,
    "--accent-soft": t.accentSoft,
    "--badge-bg": t.badgeBg,
    "--badge-ink": t.badgeInk,
    "--snap-fill": t.snapFill,
    "--snap-edge": t.snapEdge,
    "--danger-soft": t.dangerSoft,
    "--danger-ink": t.dangerInk,
    "--font-sans": `"Inter", system-ui, sans-serif`,
    "--font-mono": `"JetBrains Mono", ui-monospace, monospace`,
  };
  for (const level of ELEVATIONS) vars[`--elev-${ELEVATION_LEVEL[level]}`] = t.elevation[level];
  Object.assign(vars, scaleVariables());
  for (const tone of TONES) {
    vars[`--icon-${tone}`] = t.icon[tone];
    vars[`--chrome-icon-${tone}`] = t.chromeIcon[tone];
    vars[`--chrome-pill-${tone}-ink`] = t.chromePillInk[tone];
    vars[`--pill-${tone}-ink`] = t.pill[tone].ink;
    vars[`--pill-${tone}-bg`] = t.pill[tone].bg;
    vars[`--pill-${tone}-edge`] = t.pill[tone].edge;
  }
  return vars;
}

/** As mesmas variáveis como texto CSS (documentação, depuração e testes). */
export function themeCss(theme: ThemeName, selector = ":root"): string {
  const body = Object.entries(themeVariables(theme))
    .map(([name, value]) => `  ${name}: ${value};`)
    .join("\n");
  return `${selector} {\n${body}\n}`;
}

// --- CodeMirror (definição; o uso vem na T08) ---

export interface CodeTokens {
  text: string;
  gutter: string;
  caret: string;
  selection: string;
  focus: string;
  keyword: string;
  string: string;
  number: string;
  comment: string;
  function: string;
  type: string;
  property: string;
  tag: string;
  operator: string;
  link: string;
}

export const CODE_THEMES: Readonly<Record<ThemeName, CodeTokens>> = {
  dark: {
    text: "#E4E4E7",
    gutter: "#8B8B98",
    caret: "#C4B5FD",
    selection: "rgba(167, 139, 250, 0.25)",
    focus: "#C4B5FD",
    keyword: "#C4B5FD",
    string: "#86EFAC",
    number: "#FDBA74",
    comment: "#9C9CA8",
    function: "#7DD3FC",
    type: "#FCD34D",
    property: "#A5B4FC",
    tag: "#FDA4AF",
    operator: "#A1A1AA",
    link: "#7DD3FC",
  },
  light: {
    text: "#26262F",
    gutter: "#6B6B78",
    caret: BRAND_DARK.violet,
    selection: "rgba(109, 40, 217, 0.15)",
    focus: BRAND_DARK.violet,
    keyword: BRAND_DARK.violet,
    string: "#15803D",
    number: "#A0410A",
    comment: "#5B5B66",
    function: "#0369A1",
    type: BRAND_DARK.amber,
    property: "#4338CA",
    tag: "#BE123C",
    operator: "#52525B",
    link: "#0369A1",
  },
};

// --- Favicon de atenção ---

export const FAVICON_TOKENS = {
  bg: "#0D0D15",
  dot: "#F59E0B",
  brand: BRAND,
} as const;

// --- Imagens anexadas ---

/** Fundo no lugar do canal alfa ao reencodar um anexo em JPEG (que não tem transparência). */
export const JPEG_FLATTEN_BACKGROUND = "#FFFFFF";
