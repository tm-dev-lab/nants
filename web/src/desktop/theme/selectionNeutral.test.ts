// @vitest-environment node
/*
 * Seleção e foco sem violeta (spec 095, decisão 2; pedido do usuário em 2026-10-20, no desktop E no
 * celular): nenhum componente selecionado, atual ou focado desenha borda, contorno ou anel violeta.
 * A seleção aparece por fundo (`--accent-soft` no desktop; `--lane`/`--state-press` no celular) e
 * pelo peso do texto; o foco de teclado usa o anel neutro (`--focus-outer`/`--focus-inner`).
 *
 * As regras são lidas de todo CSS de `src/` (desktop, `mobile.css` e as janelas), inclusive dentro
 * de `@media`. Exceções motivadas ficam em `EXCEPTIONS` (hoje nenhuma).
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRules } from "../test-utils";
import { parseColor } from "./contrast";
import { THEME_NAMES, THEMES, themeVariables } from "./tokens";

const SRC = resolve(process.cwd(), "src");

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (name.endsWith(".css")) out.push(full);
  }
  return out;
}

const sheets = walk(SRC).map((full) => ({ file: relative(SRC, full).split(sep).join("/"), css: readFileSync(full, "utf8") }));

/** Seletor de estado de seleção, item atual ou foco. */
const STATE =
  /:focus(?:-visible|-within)?\b|\[aria-(?:selected|current|pressed|checked)[=\]]|\[data-(?:checked|active|selected|current)="true"\]|\[data-state="focused"\]|\[data-indicator="focused"\]/;

/** Cores violetas: tokens de destaque e tons violeta, e literais. */
const VIOLET = /var\(--(?:accent(?:-[a-z]+)?|icon-violet|chrome-icon-violet|pill-violet-[a-z]+|chrome-pill-violet-ink|snap-(?:edge|fill))\)|#(?:7c3aed|6d28d9|a78bfa|c4b5fd|8b5cf6|ddd6fe|ede9fe)\b|rgba\(\s*(?:167,\s*139,\s*250|109,\s*40,\s*217|124,\s*58,\s*237)/i;

/** Declarações que desenham contorno: borda, contorno e sombra (anel). */
const EDGE_DECL = /(?:^|;)\s*((?:border(?:-(?!radius)[a-z]+)*)|outline(?:-color)?|box-shadow)\s*:\s*([^;]+);/g;

/** Exceções motivadas (seletor → motivo). Vazio: nenhum caso foi indispensável. */
const EXCEPTIONS: ReadonlyMap<string, string> = new Map();

/** Violações numa folha de estilos (exportável para a amostra). */
function offenders(file: string, css: string): string[] {
  const out: string[] = [];
  for (const rule of cssRules(css)) {
    const states = rule.selectors.filter((s) => STATE.test(s) && !EXCEPTIONS.has(s));
    if (states.length === 0) continue;
    for (const m of rule.body.matchAll(EDGE_DECL)) {
      if (VIOLET.test(m[2] ?? "")) out.push(`${file}: ${rule.at ?? ""} ${states.join(", ")} ${m[1]}: ${(m[2] ?? "").trim()}`);
    }
  }
  return out;
}

/** "Neutro": diferença entre os canais RGB pequena (cinza, sem matiz). */
function neutral(color: string): boolean {
  const c = parseColor(color);
  return Math.max(c.r, c.g, c.b) - Math.min(c.r, c.g, c.b) <= 16;
}

describe("seleção e foco sem violeta em borda, contorno ou anel (desktop e celular)", () => {
  it("nenhuma regra de seleção, item atual ou foco usa violeta em border, outline ou box-shadow", () => {
    const all = sheets.flatMap(({ file, css }) => offenders(file, css));
    expect(all).toEqual([]);
  });

  it("a checagem pega borda, contorno e anel violeta em seleção e foco, inclusive no celular", () => {
    const sample = [
      '.nd-root .nd-switcher-item[aria-current="true"] { border-color: var(--accent); }',
      '@media (x) { .nd-root[data-mobile="true"] .nd-tab:focus-visible { outline: 2px solid var(--icon-violet); } }',
      '.nd-card[data-selected="true"] { box-shadow: 0 0 0 1px var(--pill-violet-edge); }',
      ".nd-ok[aria-selected] { background: var(--accent-soft); }",
      ".nd-plain { border: 1px solid var(--accent); }",
    ].join(" ");
    expect(offenders("amostra.css", sample)).toHaveLength(3);
  });

  it("contraste alto (095/T09): a aba atual e o item atual do Alternador ganham o contorno --edge-strong, nunca violeta", () => {
    const desktop = sheets.find((s) => s.file === "desktop/theme/desktop.css");
    if (desktop === undefined) throw new Error("desktop.css ausente");
    const more = cssRules(desktop.css).filter((r) => r.at?.includes("prefers-contrast: more") === true);
    const outlined = more.filter((r) => /outline:\s*1px solid var\(--edge-strong\)/.test(r.body)).flatMap((r) => r.selectors);
    for (const current of ['.nd-mobile-tab[aria-current="page"]', '.nd-switcher-item[aria-current="true"]']) {
      expect(outlined.some((s) => s.includes(current))).toBe(true);
    }
    const all = more.flatMap((r) => r.selectors.filter((s) => /nd-(?:mobile-tab|switcher-item)\[aria-current/.test(s)).map(() => r.body));
    expect(all.length).toBeGreaterThan(0);
    for (const body of all) expect(VIOLET.test(body)).toBe(false);
  });

  it("os tokens da borda de seleção e do anel de foco são neutros nos dois temas", () => {
    for (const theme of THEME_NAMES) {
      const t = THEMES[theme];
      for (const [name, value] of [
        ["selectedEdge", t.selectedEdge],
        ["focusOuter", t.focusOuter],
        ["focusInner", t.focusInner],
        ["chromeInk", t.chromeInk],
      ] as const) {
        expect({ theme, name, neutral: neutral(value) }).toEqual({ theme, name, neutral: true });
      }
      // O anel de selecionado usa a borda de seleção (neutra).
      expect(themeVariables(theme)["--ring-selected"]).toContain("var(--selected-edge)");
    }
  });

  it("nenhum CSS redefine a borda de seleção ou o anel de foco com violeta (o titânio usa --chrome-ink)", () => {
    const bad: string[] = [];
    for (const { file, css } of sheets) {
      for (const m of css.matchAll(/(--(?:selected-edge|focus-outer|focus-inner|ring-selected))\s*:\s*([^;]+);/g)) {
        if (VIOLET.test(m[2] ?? "")) bad.push(`${file}: ${m[1]}: ${m[2]}`);
      }
    }
    expect(bad).toEqual([]);
  });
});
