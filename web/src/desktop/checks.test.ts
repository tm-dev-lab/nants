// @vitest-environment node
/*
 * Checagens automatizadas da interface (spec 093, decisões 13 e 14), em TODO o `web/src`:
 * 1. cor só nos tokens: nenhuma classe de paleta Tailwind, hex literal ou função de cor
 *    (`rgb()`, `hsl()`...) fora de `desktop/theme/tokens.ts`;
 * 2. ícones, nunca caracteres: nenhum caractere proibido (ou emoji) como conteúdo de botão, item
 *    de menu ou selo; e nenhum texto que use um deles como ícone (por exemplo "+ Novo" ou "Abrir →").
 *    Vale para o código dos componentes (os nomes dos testes não são interface). O texto dentro
 *    de `<kbd>` é rótulo de tecla ("Ctrl+K", "Alt+1…9") e fica de fora. A varredura do DOM
 *    montado (`findForbiddenGlyphs`, em `glyphs.ts`) complementa esta nos testes dos componentes.
 *
 * 3. escala de acabamento (spec 094, T06): raio, sombra, fonte, gap e padding só pelos tokens, e
 *    ícones só nos tamanhos da escala (seção no fim do arquivo).
 * 4. estilo do texto (spec 095, T08): nenhum texto da interface (literais, templates, texto JSX e
 *    atributos) com travessão, meia-risca ou emoji, nem com a sigla antiga da aprovação automática.
 *    Exceções exatas em `STYLE_EXEMPT`, cada uma com o motivo.
 *
 * A varredura usa a AST (parser do Rolldown, que já vem com o Vite): olha só o que é texto
 * (literais, templates e texto JSX), nunca comentários.
 */
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { parseAst } from "rolldown/parseAst";
import { describe, expect, it } from "vitest";
import { EMOJI, FORBIDDEN_GLYPHS, forbiddenGlyphIn } from "./glyphs";
import { ICON_SIZES, themeVariables } from "./theme/tokens";

const SRC = resolve(process.cwd(), "src");
const ROOTS = [SRC];

/** Arquivos isentos, com o motivo. */
const COLOR_EXEMPT: ReadonlyMap<string, string> = new Map([
  ["desktop/theme/tokens.ts", "a fonte única das cores"],
  ["desktop/theme/contrast.test.ts", "testa a leitura de cores (hex e rgba)"],
  ["desktop/checks.test.ts", "contém os exemplos que as checagens precisam pegar"],
]);
const GLYPH_EXEMPT: ReadonlyMap<string, string> = new Map([
  ["desktop/glyphs.ts", "a lista dos caracteres proibidos"],
  ["reviewMessage.ts", "lê o formato da mensagem do Revisor (marcas de critério e separadores), não desenha controle"],
  ["reportMarkdown.ts", "lê markdown (marcador de lista) e trunca a prosa longa com reticências"],
]);

function files(dir: string): string[] {
  if (!existsSync(dir)) return [];
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...files(full));
    else if (/\.(ts|tsx|css)$/.test(name)) out.push(full);
  }
  return out;
}

const rel = (full: string): string => relative(SRC, full).split(sep).join("/");

// --- Regras ---

const PALETTE =
  /\b(?:text|bg|border|ring|fill|stroke|from|to|via)-(?:zinc|slate|gray|neutral|white|black|indigo|violet|purple|cyan|sky|amber|yellow|rose|red|pink|emerald|green|teal)\b/;
const HEX = /(?<![\w&])#(?:[0-9a-f]{8}|[0-9a-f]{6}|[0-9a-f]{3,4})(?![\w-])/i;
const COLOR_FN = /\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch|color-mix)\(/i;

/** Problema de cor num trecho de texto, ou `null`. */
export function colorProblem(text: string): string | null {
  const palette = PALETTE.exec(text);
  if (palette !== null) return `classe de paleta "${palette[0]}"`;
  const hex = HEX.exec(text);
  if (hex !== null) return `hex "${hex[0]}"`;
  const fn = COLOR_FN.exec(text);
  if (fn !== null) return `função de cor "${fn[0]}"`;
  return null;
}

/** Caracteres que nunca aparecem em texto da interface (só ícones). */
const ALWAYS_ICON = FORBIDDEN_GLYPHS.filter((g) => g !== "+" && g !== "—" && g !== "…");
/** "+", "—" e "…" valem em prosa e em rótulos de tecla; como ícone, só nas pontas do texto. */
const EDGE_ONLY = FORBIDDEN_GLYPHS.filter((g) => !ALWAYS_ICON.includes(g));

/** Texto qualquer (literal, template, JSX) que usa caractere no lugar de ícone. */
export function glyphAsIcon(text: string): string | null {
  for (const glyph of ALWAYS_ICON) if (text.includes(glyph)) return glyph;
  const emoji = EMOJI.exec(text);
  if (emoji !== null) return emoji[0];
  const trimmed = text.trim();
  for (const glyph of EDGE_ONLY) {
    if (trimmed === glyph || trimmed.startsWith(`${glyph} `) || trimmed.endsWith(` ${glyph}`)) return glyph;
  }
  return null;
}

// --- Varredura da AST ---

type Node = Record<string, unknown> & { type?: string };

const CONTROL_ROLES = new Set(["button", "menuitem", "menuitemradio", "menuitemcheckbox", "tab", "option"]);

function jsxName(opening: Node): string {
  const name = opening.name as Node | undefined;
  if (name?.type === "JSXIdentifier") return String(name.name);
  return "";
}

function attr(opening: Node, attrName: string): Node | null {
  const attrs = (opening.attributes as Node[] | undefined) ?? [];
  for (const a of attrs) {
    if (a.type === "JSXAttribute" && (a.name as Node | undefined)?.name === attrName) return a;
  }
  return null;
}

function isControl(opening: Node): boolean {
  if (jsxName(opening) === "button") return true;
  if (attr(opening, "data-badge") !== null) return true;
  const role = attr(opening, "role")?.value as Node | undefined;
  return role?.type === "Literal" && typeof role.value === "string" && CONTROL_ROLES.has(role.value);
}

export interface SourceIssue {
  file: string;
  line: number;
  problem: string;
}

function lineOf(source: string, offset: unknown): number {
  if (typeof offset !== "number") return 0;
  return source.slice(0, offset).split("\n").length;
}

/** Textos de um arquivo TS/TSX com o contexto (dentro de controle? dentro de `<kbd>`?). */
function visitTexts(source: string, lang: "ts" | "tsx", onText: (text: string, offset: unknown, inControl: boolean) => void): void {
  const program = parseAst(source, { lang }) as unknown as Node;
  const visit = (node: unknown, inControl: boolean, inKbd: boolean): void => {
    if (Array.isArray(node)) {
      for (const child of node) visit(child, inControl, inKbd);
      return;
    }
    if (typeof node !== "object" || node === null) return;
    const n = node as Node;
    switch (n.type) {
      case "Literal":
        if (typeof n.value === "string") onText(n.value, n.start, inControl && !inKbd);
        return;
      case "TemplateElement": {
        const value = n.value as { cooked?: string | null; raw?: string } | undefined;
        onText(value?.cooked ?? value?.raw ?? "", n.start, inControl && !inKbd);
        return;
      }
      case "JSXText":
        onText(String(n.value), n.start, inControl && !inKbd);
        return;
      case "JSXAttribute":
        // Atributos não são conteúdo (className, aria-label...), mas os textos ainda contam para a cor.
        visit(n.value, false, inKbd);
        return;
      case "JSXElement": {
        const opening = n.openingElement as Node;
        visit(opening, inControl, inKbd);
        const control = inControl || isControl(opening);
        const kbd = inKbd || jsxName(opening) === "kbd";
        visit(n.children, control, kbd);
        return;
      }
      default:
        for (const [key, value] of Object.entries(n)) {
          if (key === "start" || key === "end" || key === "type") continue;
          visit(value, inControl, inKbd);
        }
    }
  };
  visit(program, false, false);
}

export function scanSource(file: string, source: string): { color: SourceIssue[]; glyph: SourceIssue[] } {
  const color: SourceIssue[] = [];
  const glyph: SourceIssue[] = [];
  if (file.endsWith(".css")) {
    const stripped = source.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
    stripped.split("\n").forEach((text, i) => {
      const problem = colorProblem(text);
      if (problem !== null) color.push({ file, line: i + 1, problem });
    });
    return { color, glyph };
  }
  visitTexts(source, file.endsWith(".tsx") ? "tsx" : "ts", (text, offset, inControl) => {
    const problem = colorProblem(text);
    if (problem !== null) color.push({ file, line: lineOf(source, offset), problem });
    const asIcon = inControl ? forbiddenGlyphIn(text) : glyphAsIcon(text);
    if (asIcon !== null) {
      glyph.push({
        file,
        line: lineOf(source, offset),
        problem: `caractere "${asIcon}" ${inControl ? "como conteúdo de controle, menu ou selo" : "no lugar de um ícone"}`,
      });
    }
  });
  return { color, glyph };
}

function scanAll(): { color: SourceIssue[]; glyph: SourceIssue[] } {
  const color: SourceIssue[] = [];
  const glyph: SourceIssue[] = [];
  for (const full of ROOTS.flatMap(files)) {
    const file = rel(full);
    const result = scanSource(file, readFileSync(full, "utf8"));
    if (!COLOR_EXEMPT.has(file)) color.push(...result.color);
    // Os nomes dos testes não são interface: a regra dos ícones vale para o código dos componentes.
    if (!GLYPH_EXEMPT.has(file) && !/\.test\.tsx?$/.test(file)) glyph.push(...result.glyph);
  }
  return { color, glyph };
}

const format = (issues: SourceIssue[]): string[] => issues.map((i) => `${i.file}:${i.line} ${i.problem}`);

describe("checagem de cor: só tokens", () => {
  it("pega classe de paleta, hex e função de cor, e ignora comentários", () => {
    const bad = [
      'export const A = <div className="bg-zinc-900 p-2" />;',
      'export const B = <div className="hover:text-white" />;',
      'const c = "#0E7490";',
      "const d = `border: 1px solid #fff`;",
      'const e = { color: "rgba(0, 0, 0, 0.5)" };',
    ];
    for (const source of bad) expect(scanSource("x.tsx", source).color).toHaveLength(1);
    const good = [
      "// #0E7490 em comentário não conta\nconst a = 1;",
      'const b = <a href="#ajuda" className="nd-card text-[var(--ink)] bg-[var(--surface)]" />;',
      'const c = "var(--accent)";',
    ];
    for (const source of good) expect(scanSource("x.tsx", source).color).toEqual([]);
    expect(scanSource("x.css", ".a { color: #fff; }\n/* #000 */").color).toHaveLength(1);
    expect(scanSource("x.css", ".a { color: var(--ink); }").color).toEqual([]);
  });

  it("nenhum arquivo de web/src tem cor fora dos tokens", () => {
    expect(format(scanAll().color)).toEqual([]);
  });
});

describe("checagem de ícones: nunca caracteres", () => {
  const [times, , dash, , , triDown, , arrow, , check, , bullet, plus, ellipsis] = FORBIDDEN_GLYPHS;

  it("pega caractere proibido em botão, item de menu e selo, mas não em <kbd>", () => {
    const bad = [
      `const a = <button>${times}</button>;`,
      `const b = <div role="menuitem">Abrir ${arrow}</div>;`,
      `const c = <span data-badge>${bullet}</span>;`,
      `const d = <button>{open ? "${triDown}" : "x"}</button>;`,
      `const e = <button><span>Mais${ellipsis}</span></button>;`,
      `const f = <button>${plus} Novo</button>;`,
      `const g = { label: "${check} Feito" };`,
      `const h = { label: "${plus} Novo projeto" };`,
      'const i = <button>Ok \u{1F680}</button>;',
    ];
    for (const source of bad) expect({ source, issues: scanSource("x.tsx", source).glyph.length }).toEqual({ source, issues: 1 });
    const good = [
      `const a = <button>Abrir <kbd>Ctrl${plus}K</kbd></button>;`,
      `const b = <p>Texto ${dash} com travessão em prosa.</p>;`,
      `const c = { keys: "Alt${plus}1${ellipsis}9" };`,
      `const d = <p>Carregando${ellipsis}</p>;`,
      `// comentário com ${arrow} não conta\nconst e = 1;`,
    ];
    for (const source of good) expect({ source, issues: scanSource("x.tsx", source).glyph }).toEqual({ source, issues: [] });
  });

  it("nenhum código de web/src usa caractere no lugar de ícone", () => {
    expect(format(scanAll().glyph)).toEqual([]);
  });
});

// --- Estilo do texto (spec 095, T08) ---

const EM_DASH = "—";
const EN_DASH = "–";
/** Sigla antiga da aprovação automática (decisão 12): o texto visível nunca a usa. */
const OLD_AUTO_APPROVAL = "YO" + "LO";

/** Problema de estilo num texto da interface, ou `null`. */
export function styleProblem(text: string): string | null {
  if (text.includes(EM_DASH)) return "travessão";
  if (text.includes(EN_DASH)) return "meia-risca";
  const emoji = EMOJI.exec(text);
  if (emoji !== null) return `emoji "${emoji[0]}"`;
  if (text.includes(OLD_AUTO_APPROVAL)) return `"${OLD_AUTO_APPROVAL}" (use "aprovação automática")`;
  return null;
}

/** Arquivos inteiros isentos da regra de estilo, com o motivo. */
const STYLE_EXEMPT_FILES: ReadonlyMap<string, string> = new Map([
  ["desktop/glyphs.ts", "a lista dos caracteres proibidos (inclui o travessão)"],
]);

/** Literais exatos isentos, com o motivo: só leitura de mensagens antigas, nunca exibidos. */
const STYLE_EXEMPT: readonly { file: string; text: string; reason: string }[] = [
  {
    file: "reviewMessage.ts",
    text: ` ${EM_DASH} `,
    reason: "lê o separador antigo das mensagens do Revisor gravadas antes da 095/T08; a tela não o mostra",
  },
  {
    file: "apps/conversation/Bubble.tsx",
    text: `aprovado automaticamente (${OLD_AUTO_APPROVAL})`,
    reason: "reconhece o prefixo antigo das aprovações já gravadas; a pílula mostra o texto novo",
  },
  {
    file: "apps/conversation/Bubble.tsx",
    text: `[rodada autônoma ${OLD_AUTO_APPROVAL}]`,
    reason: "reconhece a marca antiga da rodada autônoma e a troca pela nova na exibição",
  },
];

function styleIssues(): { issues: string[]; unused: string[] } {
  const issues: string[] = [];
  const used = new Set<number>();
  for (const full of files(SRC).filter((f) => /\.tsx?$/.test(f) && !/\.test\.tsx?$/.test(f))) {
    const file = rel(full);
    if (STYLE_EXEMPT_FILES.has(file)) continue;
    const source = readFileSync(full, "utf8");
    visitTexts(source, file.endsWith(".tsx") ? "tsx" : "ts", (text, offset) => {
      const problem = styleProblem(text);
      if (problem === null) return;
      const k = STYLE_EXEMPT.findIndex((e) => e.file === file && e.text === text);
      if (k >= 0) {
        used.add(k);
        return;
      }
      issues.push(`${file}:${lineOf(source, offset)} ${problem}: ${JSON.stringify(text.slice(0, 80))}`);
    });
  }
  const unused = STYLE_EXEMPT.filter((_, k) => !used.has(k)).map((e) => `${e.file}: ${e.text}`);
  return { issues, unused };
}

describe("estilo do texto: sem travessão, meia-risca, emoji nem a sigla antiga (095/T08)", () => {
  it("pega travessão, meia-risca, emoji e a sigla; aceita hífen, setas e marcas de critério", () => {
    for (const bad of [`a ${EM_DASH} b`, `1${EN_DASH}2`, "pronto \u{1F680}", "ok ✅", `Modo ${OLD_AUTO_APPROVAL}`]) {
      expect({ bad, problem: styleProblem(bad) === null }).toEqual({ bad, problem: false });
    }
    for (const good of ["e-mail", "Sem preço", "aprovação automática", "✓ feito", "a → b", "yolo-badge"]) {
      expect({ good, problem: styleProblem(good) }).toEqual({ good, problem: null });
    }
    // Comentários não contam; literais, texto JSX e atributos contam.
    const texts: string[] = [];
    visitTexts(`// ${EM_DASH} comentário\nconst a = <p title="t ${EM_DASH} x">b ${EN_DASH} c</p>;`, "tsx", (t) => texts.push(t));
    expect(texts.filter((t) => styleProblem(t) !== null)).toHaveLength(2);
  });

  it("cada exceção tem o motivo escrito e ainda é usada", () => {
    for (const e of STYLE_EXEMPT) expect(e.reason.length).toBeGreaterThan(20);
    for (const [, reason] of STYLE_EXEMPT_FILES) expect(reason.length).toBeGreaterThan(20);
    expect(styleIssues().unused).toEqual([]);
  });

  it("nenhum texto de web/src tem travessão, meia-risca, emoji ou a sigla antiga", () => {
    expect(styleIssues().issues).toEqual([]);
  });
});

// --- Escala de acabamento (spec 094, T06) ---
/*
 * 3. Nenhum `border-radius`, `box-shadow`, `font-size`/`font`/`font-weight`, `gap` ou `padding`
 *    solto nos CSS da interface (`desktop/**` e `apps/**`): só tokens da escala (`--r-*`,
 *    `--sp-*`, `--fs-*`, `--fw-*`, `--elev-*`, `--bevel`, `--ring-*`, `--card-edge`), `0`, `none`,
 *    palavras-chave e `calc`/`max`/`min` sobre
 *    tokens. Apelidos locais (`--pill-pad: var(--sp-1)`) valem se apontam para a escala da mesma
 *    família. As exceções ficam em `SCALE_EXCEPTIONS`, cada uma com o motivo.
 * 4. Ícones: só os tamanhos da escala (14/16/18/20) e o traço único (`ICON_STROKE`).
 */

type ScaleFamily = "radius" | "space" | "font" | "shadow";

/** Família de tokens de cada propriedade checada. */
function familyOf(prop: string): ScaleFamily | null {
  if (/^border(-(top|bottom)-(left|right))?-radius$/.test(prop)) return "radius";
  if (/^padding(-(top|right|bottom|left|inline|block)(-(start|end))?)?$/.test(prop) || /^(row-|column-)?gap$/.test(prop)) return "space";
  if (prop === "font-size" || prop === "font" || prop === "font-weight") return "font";
  if (prop === "box-shadow") return "shadow";
  return null;
}

/** Prefixos de variável aceitos por família (todas definidas em `tokens.ts`). */
const FAMILY_VARS: Readonly<Record<ScaleFamily, RegExp>> = {
  radius: /^--r-/,
  space: /^--sp-/,
  font: /^--(fs|fw|font)-/,
  // `--card-edge` (095/T01): o relevo sutil do cartão, sem contorno, por tema.
  shadow: /^--(elev-|bevel$|ring-|card-edge$)/,
};

/** Variáveis gravadas pelos componentes em tempo de execução (não vêm dos tokens nem do CSS). */
const RUNTIME_SET_VARS: ReadonlyMap<string, string> = new Map([
  ["--app-height", "altura da viewport visual, gravada em <html> por `viewport.ts` (082/T06)"],
  ["--nd-level", "profundidade do item na árvore de Arquivos, posta no `style` pelo `FileTree`"],
]);

/** Variáveis de execução (postas pelo componente), sem unidade: multiplicam um token. */
const RUNTIME_VARS: ReadonlyMap<string, string> = new Map([
  ["--nd-level", "profundidade do item na árvore de Arquivos (número sem unidade, multiplica `--sp-4`)"],
]);

/** Exceções explícitas: propriedade, valor (regex do valor inteiro) e o motivo. */
export const SCALE_EXCEPTIONS: readonly { prop: RegExp; value: RegExp; reason: string }[] = [
  {
    prop: /^box-shadow$/,
    value: /^(inset )?0 0 0 [24]px var\(--focus-inner\)$/,
    reason:
      "anel de foco duplo da 093 (claro + escuro): geometria fixa, coberta pelos testes de foco e de contraste; não é elevação nem acabamento",
  },
];

interface Declaration {
  file: string;
  line: number;
  prop: string;
  value: string;
}

/** Declarações de um CSS (comentários fora), com a linha. */
export function cssDeclarations(file: string, css: string): Declaration[] {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
  const out: Declaration[] = [];
  for (const m of text.matchAll(/(?<=[{;\s])(--[\w-]+|[a-z-]+)\s*:\s*([^;{}]+);/g)) {
    out.push({ file, line: text.slice(0, m.index).split("\n").length, prop: m[1] ?? "", value: (m[2] ?? "").replace(/\s+/g, " ").trim() });
  }
  return out;
}

/** Problema de escala num valor, ou `null`. `aliases`: apelidos locais (`--x: valor`). */
export function scaleProblem(prop: string, value: string, aliases: ReadonlyMap<string, string[]> = new Map()): string | null {
  const family = familyOf(prop);
  if (family === null) return null;
  if (SCALE_EXCEPTIONS.some((e) => e.prop.test(prop) && e.value.test(value))) return null;
  // Raio concêntrico: o externo menos o padding entre os dois (`calc(<raio> - <espaço>)`).
  const nested = family === "radius" ? /^calc\((.+) - (.+)\)$/.exec(value) : null;
  if (nested !== null) return scaleProblem("border-radius", nested[1] ?? "", aliases) ?? scaleProblem("padding", nested[2] ?? "", aliases);
  const check = (v: string, seen: ReadonlySet<string>): string | null => {
    for (const m of v.matchAll(/var\((--[\w-]+)(?:,\s*([^()]*))?\)/g)) {
      const name = m[1] ?? "";
      const fallback = m[2]?.trim();
      if (RUNTIME_VARS.has(name)) {
        // Variável de execução: o reserva só pode ser um inteiro sem unidade (multiplicador).
        if (fallback !== undefined && !/^\d+$/.test(fallback)) return `reserva solta "${fallback}" em ${name}`;
        continue;
      }
      if (FAMILY_VARS[family].test(name)) {
        // O reserva de um token também passa pela escala (`var(--sp-2, 7px)` é erro).
        if (fallback !== undefined) {
          const problem = check(fallback, seen);
          if (problem !== null) return `reserva de ${name}: ${problem}`;
        }
        continue;
      }
      const defs = aliases.get(name);
      if (defs === undefined || seen.has(name)) return `variável fora da escala "${name}"`;
      for (const def of defs) {
        const problem = check(def, new Set([...seen, name]));
        if (problem !== null) return `${name}: ${problem}`;
      }
    }
    let rest = v.replace(/var\([^()]*\)/g, " ").replace(/env\([^()]*\)/g, " ");
    // Altura de linha depois da barra (`font: var(--fs-body)/1.5`) é sem unidade e não é tamanho.
    if (family === "font") rest = rest.replace(/\/\s*[\d.]+(?![\w%])/g, " ");
    const loose = /(?<![\w-])(\d*\.?\d+)(px|em|rem|%|vh|vw|pt|ch|ex)\b/.exec(rest);
    if (loose !== null) return `valor solto "${loose[0]}"`;
    // Raio e espaço: nenhum número sobra fora dos tokens, a não ser o `0` (pega unidades fora da
    // lista, como `vmin`, `dvh`, `lh` e `cqw`, e multiplicadores como `calc(1.5 * var(--sp-1))`).
    if (family === "radius" || family === "space") {
      const digit = /(?<![\w.-])-?\d*\.?\d+[a-z%]*/gi;
      for (const n of rest.match(digit) ?? []) if (n !== "0") return `valor solto "${n}"`;
    }
    if (family === "shadow" && /\d/.test(rest)) return `sombra solta "${v}"`;
    // Peso de fonte só pelos tokens (`--fw-*`): nenhum número sobra na fonte.
    if (family === "font" && /\d/.test(rest)) return `fonte solta "${v}"`;
    return null;
  };
  return check(value, new Set());
}

const SCALE_ROOTS = ["desktop", "apps"].map((d) => join(SRC, d));
const scaleCssFiles = (): string[] => SCALE_ROOTS.flatMap(files).filter((f) => f.endsWith(".css"));

function scaleIssues(): string[] {
  const issues: string[] = [];
  for (const full of scaleCssFiles()) {
    const decls = cssDeclarations(rel(full), readFileSync(full, "utf8"));
    const aliases = new Map<string, string[]>();
    for (const d of decls) if (d.prop.startsWith("--")) aliases.set(d.prop, [...(aliases.get(d.prop) ?? []), d.value]);
    for (const d of decls) {
      const problem = scaleProblem(d.prop, d.value, aliases);
      if (problem !== null) issues.push(`${d.file}:${d.line} ${d.prop}: ${problem}`);
    }
  }
  return issues;
}

describe("checagem da escala de acabamento (094/T06)", () => {
  it("pega raio, sombra, fonte, gap e padding soltos; aceita tokens, 0 e calc sobre tokens", () => {
    const bad: [string, string][] = [
      ["border-radius", "10px"],
      ["border-radius", "var(--r-md) 3px"],
      ["box-shadow", "0 12px 30px var(--shadow)"],
      ["box-shadow", "var(--bevel), 0 0 0 1px var(--accent)"],
      ["font-size", "14px"],
      ["font", "500 13px var(--font-sans)"],
      ["font", "0.88em var(--font-mono)"],
      ["font", "600 var(--fs-caption) var(--font-sans)"],
      ["font-weight", "600"],
      ["gap", "6px"],
      ["padding", "0 var(--sp-2) 0 14px"],
      ["padding-left", "calc(8px + var(--nd-level, 0) * 14px)"],
      ["gap", "var(--line)"],
      ["border-radius", "var(--sp-2)"],
      // Estrita para raio e espaço: unidades fora da lista, multiplicadores e reservas soltas.
      ["padding", "2vmin"],
      ["gap", "1dvh"],
      ["padding-top", "1lh"],
      ["padding-inline", "2cqw"],
      ["border-radius", "50%"],
      ["padding", "calc(1.5 * var(--sp-1))"],
      ["padding-top", "calc(2 * var(--sp-12))"],
      ["gap", "var(--sp-2, 7px)"],
      ["border-radius", "var(--r-md, 9px)"],
      ["padding-left", "calc(var(--sp-2) + var(--nd-level, 1.5) * var(--sp-4))"],
      ["padding-left", "calc(var(--sp-2) + var(--nd-level, 4px) * var(--sp-4))"],
    ];
    for (const [prop, value] of bad) expect({ prop, value, ok: scaleProblem(prop, value) === null }).toEqual({ prop, value, ok: false });
    const good: [string, string][] = [
      ["border-radius", "var(--r-lg)"],
      ["border-radius", "var(--r-xs) var(--r-lg) var(--r-lg) var(--r-lg)"],
      ["box-shadow", "var(--bevel), var(--elev-3)"],
      ["box-shadow", "var(--card-edge)"],
      ["box-shadow", "none"],
      ["font", "var(--fw-medium) var(--fs-body)/1.5 var(--font-sans)"],
      ["font", "inherit"],
      ["font-weight", "var(--fw-semibold)"],
      ["gap", "var(--sp-2) var(--sp-4)"],
      ["padding", "0"],
      ["padding", "max(var(--sp-2), env(safe-area-inset-top)) var(--sp-3) var(--sp-2)"],
      ["padding-top", "calc(var(--sp-12) + var(--sp-12))"],
      ["gap", "var(--sp-2, var(--sp-1))"],
      ["padding", "var(--sp-1) var(--sp-2) var(--sp-1) calc(var(--sp-2) + var(--nd-level, 0) * var(--sp-4))"],
      ["box-shadow", "0 0 0 2px var(--focus-inner)"],
      ["box-shadow", "inset 0 0 0 4px var(--focus-inner)"],
      ["width", "37px"],
    ];
    for (const [prop, value] of good) expect({ prop, value, problem: scaleProblem(prop, value) }).toEqual({ prop, value, problem: null });
    // Apelido local: vale se aponta para a escala da mesma família.
    expect(scaleProblem("padding", "var(--pill-pad)", new Map([["--pill-pad", ["var(--sp-1)"]]]))).toBeNull();
    expect(scaleProblem("padding", "var(--pill-pad)", new Map([["--pill-pad", ["5px"]]]))).not.toBeNull();
    expect(scaleProblem("padding", "var(--pill-pad)", new Map([["--pill-pad", ["var(--r-md)"]]]))).not.toBeNull();
    // Raio concêntrico: raio externo menos o padding.
    const pill = new Map([["--pill-radius", ["var(--r-lg)"]], ["--pill-pad", ["var(--sp-1)"]]]);
    expect(scaleProblem("border-radius", "calc(var(--pill-radius) - var(--pill-pad))", pill)).toBeNull();
    expect(scaleProblem("border-radius", "calc(var(--r-lg) - 5px)")).not.toBeNull();
  });

  it("cada exceção e cada variável de execução tem o motivo escrito", () => {
    for (const e of SCALE_EXCEPTIONS) expect(e.reason.length).toBeGreaterThan(20);
    for (const [, reason] of RUNTIME_VARS) expect(reason.length).toBeGreaterThan(20);
  });

  it("nenhum CSS de desktop/** e apps/** tem raio, sombra, fonte, gap ou padding fora da escala", () => {
    expect(scaleCssFiles().length).toBeGreaterThan(5);
    expect(scaleIssues()).toEqual([]);
  });

  it("toda variável da escala usada nos CSS existe nos tokens", () => {
    const known = new Set(Object.keys(themeVariables("dark")));
    const missing = new Set<string>();
    for (const full of scaleCssFiles()) {
      for (const m of readFileSync(full, "utf8").matchAll(/var\((--(?:r|sp|fs|fw|elev|ring|state|o)-[\w-]+|--bevel|--card-edge)\b/g)) {
        if (!known.has(m[1] ?? "")) missing.add(`${rel(full)}: ${m[1]}`);
      }
    }
    expect([...missing]).toEqual([]);
  });

  it("toda variável usada nos CSS existe: nos tokens, declarada num CSS ou gravada pelos componentes (095/T01)", () => {
    for (const [, reason] of RUNTIME_SET_VARS) expect(reason.length).toBeGreaterThan(20);
    const allCss = files(SRC).filter((f) => f.endsWith(".css"));
    const known = new Set<string>([...Object.keys(themeVariables("dark")), ...RUNTIME_SET_VARS.keys()]);
    for (const full of allCss) {
      for (const d of cssDeclarations(rel(full), readFileSync(full, "utf8"))) if (d.prop.startsWith("--")) known.add(d.prop);
    }
    const missing = new Set<string>();
    for (const full of scaleCssFiles()) {
      const text = readFileSync(full, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
      for (const m of text.matchAll(/var\((--[\w-]+)/g)) if (!known.has(m[1] ?? "")) missing.add(`${rel(full)}: ${m[1]}`);
    }
    expect([...missing]).toEqual([]);
    // O material da 094 saiu de vez: nenhum token nem declaração local o recria.
    for (const gone of ["--sheen", "--noise", "--hair", "--glass", "--glass-overlay", "--glass-solid", "--title-lift"]) {
      expect({ gone, known: known.has(gone) }).toEqual({ gone, known: false });
    }
  });

  it("ícones: só os tamanhos da escala (14/16/18/20) e o traço único", () => {
    const offenders: string[] = [];
    for (const full of files(SRC).filter((f) => f.endsWith(".tsx") && !f.endsWith(".test.tsx"))) {
      const source = readFileSync(full, "utf8");
      for (const m of source.matchAll(/<Icon\b[^>]*?\bsize=\{([^}]+)\}/g)) {
        for (const size of (m[1] ?? "").match(/\d+(?:\.\d+)?/g) ?? []) {
          if (!(ICON_SIZES as readonly number[]).includes(Number(size))) offenders.push(`${rel(full)}: size ${size}`);
        }
      }
      for (const m of source.matchAll(/strokeWidth[=:]\s*\{?\s*([\d.]+)/g)) offenders.push(`${rel(full)}: strokeWidth ${m[1]} (use ICON_STROKE)`);
    }
    expect(offenders).toEqual([]);
    expect(ICON_SIZES).toEqual([14, 16, 18, 20]);
  });
});
