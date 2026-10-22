// @vitest-environment node
/*
 * Janela em foco (spec 094, decisão 4; spec 095, T01): sem borda, a janela em foco se distingue
 * pela sombra mais funda (`--elev-4` contra `--elev-2`); nunca a cor de destaque. O hairline
 * (`--hair`) e o véu da barra em foco (`--title-lift`) da 094 saíram com o titânio. O anel de foco
 * do teclado não muda e o contraste do indicador de foco segue >= 3:1 nos dois temas.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../test-utils";
import { allWallpaperExtremes, chromeBackgrounds, contrastRatio, windowBackgrounds, type Rgba } from "./contrast";
import { THEME_NAMES, THEMES } from "./tokens";

const SRC = resolve(process.cwd(), "src");
const desktopCss = readFileSync(resolve(SRC, "desktop/theme/desktop.css"), "utf8");

function cssFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...cssFiles(full));
    else if (name.endsWith(".css")) out.push(full);
  }
  return out;
}

describe("borda da janela: material, nunca destaque", () => {
  it("nenhuma regra da moldura da janela usa --accent nem a cor de seleção", () => {
    const offenders: string[] = [];
    for (const file of cssFiles(SRC)) {
      for (const rule of cssRules(readFileSync(file, "utf8"))) {
        if (!rule.selectors.some((s) => /\.nd-window\b/.test(s))) continue;
        if (/var\(--accent|var\(--selected-edge\)|var\(--win-edge/.test(rule.body)) offenders.push(`${file}: ${rule.selectors.join(", ")}`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("em foco: sem borda, só a sombra mais funda; a barra de título é a mesma (titânio)", () => {
    const base = cssRuleBody(desktopCss, ".nd-window");
    expect(base).not.toMatch(/border:/);
    const active = cssRuleBody(desktopCss, '.nd-window[data-active="true"]');
    expect(active).not.toMatch(/border/);
    // Sombra mais funda: a elevação da janela em foco (`--elev-4`) passa a da janela atrás (`--elev-2`).
    expect(base).toContain("box-shadow: var(--elev-2)");
    expect(active).toContain("box-shadow: var(--elev-4)");
    const blur = (shadow: string): number => Math.max(...[...shadow.matchAll(/0 -?\d+px (\d+)px/g)].map((m) => Number(m[1])));
    for (const theme of THEME_NAMES) expect(blur(THEMES[theme].elevation.front)).toBeGreaterThan(blur(THEMES[theme].elevation.back));
    // O véu da barra em foco (`--title-lift`) saiu: nenhuma regra pinta a barra pela janela em foco.
    expect(cssRules(desktopCss).filter((r) => r.selectors.some((s) => s.includes('.nd-window[data-active="true"] .nd-titlebar')))).toEqual([]);
  });

  it("barra de título: anel neutro e discreto DENTRO da barra; foco programático sem anel (095/T07)", () => {
    const ring = cssRules(desktopCss).filter((r) => r.selectors.includes(".nd-root .nd-titlebar:focus-visible"));
    expect(ring.map((r) => r.body.replace(/\s+/g, " ").trim())).toEqual([
      "outline: 2px solid var(--focus-outer); outline-offset: -2px; box-shadow: var(--bevel);",
    ]);
    const quiet = cssRules(desktopCss).filter((r) => r.selectors.includes('.nd-root .nd-titlebar[data-quiet-focus="true"]:focus-visible'));
    expect(quiet.map((r) => r.body.replace(/\s+/g, " ").trim())).toEqual(["outline: none; box-shadow: var(--bevel);"]);
    // O anel neutro fecha ≥ 3:1 sozinho sobre o titânio (sem o anel interno de apoio).
    for (const theme of THEME_NAMES) {
      for (const bg of chromeBackgrounds(theme)) expect(contrastRatio(THEMES[theme].focusOuter, bg)).toBeGreaterThanOrEqual(3);
    }
  });

  it("nenhuma regra da moldura (.nd-window) desenha anel ou contorno em foco", () => {
    const offenders: string[] = [];
    for (const file of cssFiles(SRC)) {
      for (const rule of cssRules(readFileSync(file, "utf8"))) {
        const framed = rule.selectors.filter((s) => /\.nd-window(?:\[[^\]]*\])*:(?:focus|focus-visible|focus-within)\b/.test(s));
        if (framed.length > 0 && /outline|box-shadow|border/.test(rule.body)) offenders.push(`${file}: ${framed.join(", ")}`);
      }
    }
    expect(offenders).toEqual([]);
  });
});

/** Especificidade de um seletor simples: [ids, classes/atributos/pseudo-classes, tipos]. */
function specificity(selector: string): [number, number, number] {
  const attrs = (selector.match(/\[[^\]]*\]/g) ?? []).length;
  const rest = selector.replace(/\[[^\]]*\]/g, " ");
  const ids = (rest.match(/#[\w-]+/g) ?? []).length;
  const classes = (rest.match(/\.[\w-]+/g) ?? []).length + (rest.match(/(?<!:):(?!:)[\w-]+/g) ?? []).length;
  const types = (rest.match(/(?:^|[\s>+~])[a-z][\w-]*/gi) ?? []).length;
  return [ids, classes + attrs, types];
}

const beats = (a: [number, number, number], b: [number, number, number]): number => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];

/** Propriedades declaradas num corpo de regra. */
const properties = (body: string): string[] =>
  body
    .split(";")
    .map((d) => d.trim().split(":")[0]?.trim() ?? "")
    .filter((p) => p !== "");

describe("preferências do sistema vencem as regras base (ordem e especificidade)", () => {
  it("em transparência reduzida e contraste alto, cada regra vence a base do mesmo elemento", () => {
    const rules = cssRules(desktopCss);
    const losers: string[] = [];
    rules.forEach((media, mediaIndex) => {
      if (media.at === null || !/prefers-reduced-transparency|prefers-contrast/.test(media.at)) return;
      for (const selector of media.selectors) {
        // Mesmo elemento: o seletor sem o prefixo `.nd-root ` usado para subir a especificidade.
        const target = selector.replace(/^\.nd-root /, "");
        rules.forEach((base, baseIndex) => {
          if (base.at !== null) return;
          for (const baseSelector of base.selectors) {
            if (baseSelector.replace(/^\.nd-root /, "") !== target) continue;
            const shared = properties(media.body).filter((p) => properties(base.body).includes(p));
            if (shared.length === 0) continue;
            const order = beats(specificity(selector), specificity(baseSelector));
            if (order < 0 || (order === 0 && mediaIndex < baseIndex)) losers.push(`${media.at} ${selector} perde para ${baseSelector}`);
          }
        });
      }
    });
    expect(losers).toEqual([]);
  });

  it("as regras das preferências não apagam um estado mais específico do mesmo elemento (ex.: anel de integrar)", () => {
    // Todas as folhas: a ordem entre arquivos não é garantida, então empate de especificidade entre
    // arquivos diferentes conta como perda (o estado precisa vencer por especificidade ou ter par no media).
    const sheets = cssFiles(SRC).map((file) => ({ file, rules: cssRules(readFileSync(file, "utf8")) }));
    const all = sheets.flatMap(({ file, rules }) => rules.map((rule, index) => ({ file, rule, index })));
    const erased: string[] = [];
    for (const media of all) {
      if (media.rule.at === null || !/prefers-reduced-transparency|prefers-contrast/.test(media.rule.at)) continue;
      for (const selector of media.rule.selectors) {
        const target = selector.replace(/^\.nd-root /, "");
        for (const base of all) {
          if (base.rule.at !== null) continue;
          for (const baseSelector of base.rule.selectors) {
            const stripped = baseSelector.replace(/^\.nd-root /, "");
            // Mesmo elemento num estado: o seletor da preferência mais um atributo ou pseudo-classe.
            if (!stripped.startsWith(target) || !/^[[:]/.test(stripped.slice(target.length))) continue;
            const shared = properties(media.rule.body).filter((p) => properties(base.rule.body).includes(p));
            if (shared.length === 0) continue;
            const order = beats(specificity(baseSelector), specificity(selector));
            const wins = order > 0 || (order === 0 && base.file === media.file && base.index > media.index);
            if (wins) continue;
            // Perde: precisa de uma regra do estado dentro do mesmo media, com especificidade maior ou igual.
            const kept = all.some(
              (m) =>
                m.rule.at === media.rule.at &&
                m.rule.selectors.some((s) => s.replace(/^\.nd-root /, "") === stripped && beats(specificity(s), specificity(selector)) > 0),
            );
            if (!kept) erased.push(`${media.rule.at} ${selector} apaga ${baseSelector} (${shared.join(", ")})`);
          }
        }
      }
    }
    expect(erased).toEqual([]);
    // O halo de integrar (no ponto da pílula, 095/T07 A-01) continua no contraste alto: nenhuma regra
    // dele mexe na sombra da pílula nem do ponto.
    const pillShadow = cssRules(desktopCss).filter(
      (r) => r.at?.includes("prefers-contrast: more") === true && r.selectors.some((s) => s.includes(".nd-run-pill") || s.includes(".nd-dot")) && /box-shadow/.test(r.body),
    );
    expect(pillShadow).toEqual([]);
    const shell = readFileSync(resolve(SRC, "desktop/theme/shell.css"), "utf8");
    expect(cssRuleBody(shell, '.nd-run-pill[data-flash="true"] .nd-dot')).toContain("box-shadow: var(--ring-flash)");
    expect(cssRules(shell).filter((r) => r.selectors.includes('.nd-run-pill[data-flash="true"]'))).toEqual([]);
  });
});

describe("tokens da janela em foco", () => {
  for (const theme of THEME_NAMES) {
    const t = THEMES[theme];
    it(`${theme}: indicador de foco (anel duplo) >= 3:1 em todo fundo, inclusive no titânio das barras`, () => {
      const backgrounds: Rgba[] = [...chromeBackgrounds(theme), ...windowBackgrounds(theme), ...allWallpaperExtremes(theme).map((e) => e.color)];
      for (const bg of backgrounds) {
        const best = Math.max(contrastRatio(t.focusOuter, bg), contrastRatio(t.focusInner, bg));
        expect(best).toBeGreaterThanOrEqual(3);
      }
      // Os dois anéis se distinguem entre si.
      expect(contrastRatio(t.focusOuter, t.focusInner)).toBeGreaterThanOrEqual(3);
    });
  }
});
