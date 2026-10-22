// @vitest-environment node
/*
 * O CSS que o build GERA mantém as propriedades padrão (spec 094, T08, V-02). O build passa cada
 * CSS por duas transformações do lightningcss:
 * 1. a do Tailwind (`optimize` de `@tailwindcss/node`, usada pelo `@tailwindcss/vite`), com os
 *    alvos dele;
 * 2. a minificação do Vite (`build.cssMinify`, que no Vite é o lightningcss), com os alvos de
 *    `build.cssTarget`. Os dois vêm do `vite.config.ts` resolvido pelo próprio Vite
 *    (`resolveConfig`, o mesmo do build), e o lightningcss é a cópia que o Vite usa (a dele, em
 *    `vite/node_modules`, pode ter outra versão que a da raiz).
 * O lightningcss junta `backdrop-filter` e `-webkit-backdrop-filter` como UMA propriedade e fica com
 * a ÚLTIMA declarada: com a prefixada por último, a padrão sumia e o Chrome/Edge (que não aplicam
 * a prefixada) nunca desfocavam o vidro. A ordem certa é a prefixada antes da padrão; com ela o
 * lightningcss gera as duas para os alvos que ainda pedem o prefixo (Safari 16.4).
 * Este teste roda a MESMA sequência sobre os CSS da interface e confere a saída.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { join, relative, resolve, sep } from "node:path";
import { optimize } from "@tailwindcss/node";
import type { Targets } from "lightningcss";
import { resolveConfig } from "vite";
import { beforeAll, describe, expect, it } from "vitest";

const WEB = process.cwd();
const SRC = resolve(WEB, "src");

/** O lightningcss que a minificação do Vite carrega (resolvido a partir do pacote do Vite). */
const viteLightning = createRequire(resolve(WEB, "node_modules/vite/package.json"))("lightningcss") as typeof import("lightningcss");

/** Navegador do alvo do Vite (`chrome111`, `safari16.4`, `ios16.4`) no nome do lightningcss. */
const BROWSER: Readonly<Record<string, keyof Targets>> = { chrome: "chrome", edge: "edge", firefox: "firefox", safari: "safari", ios: "ios_saf", opera: "opera" };

/** Converte a lista `build.cssTarget` do Vite em alvos do lightningcss (`major << 16 | minor << 8`). */
function toTargets(list: readonly string[]): Targets {
  const out: Targets = {};
  for (const item of list) {
    const m = /^([a-z]+)(\d+)(?:\.(\d+))?$/.exec(item);
    const browser = m === null ? undefined : BROWSER[m[1] ?? ""];
    if (m === null || browser === undefined) throw new Error(`alvo do Vite não reconhecido: ${item}`);
    out[browser] = (Number(m[2]) << 16) | (Number(m[3] ?? 0) << 8);
  }
  return out;
}

let viteTargets: Targets = {};
let cssMinify: unknown;

beforeAll(async () => {
  // A mesma configuração do `vite build` (lê o `vite.config.ts`).
  const config = await resolveConfig({ configFile: resolve(WEB, "vite.config.ts") }, "build", "production", "production");
  cssMinify = config.build.cssMinify;
  const target = config.build.cssTarget;
  if (!Array.isArray(target)) throw new Error(`build.cssTarget inesperado: ${String(target)}`);
  viteTargets = toTargets(target as string[]);
}, 60_000);

/** A sequência do build: Tailwind e depois a minificação do Vite. */
function buildCss(css: string, file: string): string {
  const tailwind = optimize(css, { file, minify: false }).code;
  return viteLightning.transform({ filename: file, code: Buffer.from(tailwind), minify: true, targets: viteTargets }).code.toString();
}

function walk(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    if (statSync(full).isDirectory()) out.push(...walk(full));
    else if (name.endsWith(".css")) out.push(full);
  }
  return out;
}

const files = [...["desktop", "apps"].flatMap((d) => walk(join(SRC, d))), join(SRC, "styles.css")].map((full) => ({
  file: relative(SRC, full).split(sep).join("/"),
  css: readFileSync(full, "utf8"),
}));
const desktop = files.find((f) => f.file === "desktop/theme/desktop.css");
let desktopOut = "";
beforeAll(() => {
  desktopOut = desktop === undefined ? "" : buildCss(desktop.css, desktop.file);
});

describe("V-02: a sequência do teste é a do build", () => {
  it("o build minifica o CSS com o lightningcss (cssMinify não é esbuild nem desligado) e há alvos", () => {
    // Se o vite.config.ts passar a usar `cssMinify: "esbuild"` (ou desligar), este teste deixa de
    // reproduzir o build: ajuste `buildCss` junto.
    expect(cssMinify === true || cssMinify === "lightningcss").toBe(true);
    expect(Object.keys(viteTargets).length).toBeGreaterThan(0);
  });

  it("styles.css (o reset do Tailwind) entra na varredura", () => {
    expect(files.some((f) => f.file === "styles.css")).toBe(true);
  });
});

/** Corpo da regra minificada com exatamente este seletor (a primeira, ou a de dentro de `at`). */
function ruleOut(out: string, selector: string, at?: string): string {
  const scope = at === undefined ? out : out.slice(out.indexOf(at));
  const start = scope.indexOf(`${selector}{`);
  if (start === -1) throw new Error(`regra ausente na saída: ${selector}`);
  const open = start + selector.length + 1;
  return scope.slice(open, scope.indexOf("}", open));
}

describe("V-02: o CSS gerado mantém as formas padrão das propriedades prefixadas", () => {
  /*
   * 095/T01: o vidro saiu (titânio opaco), e com ele o `backdrop-filter` das superfícies. O que
   * fica é o titânio no CSS final e a regra geral de não perder a forma padrão no build.
   */
  it("o titânio sai no CSS final: --chrome com o bisel, sem desfoque", () => {
    const body = ruleOut(desktopOut, ".nd-chrome");
    expect(body).toContain("background:var(--chrome)");
    expect(body).toContain("box-shadow:var(--bevel)");
    expect(desktopOut).not.toContain("backdrop-filter");
    const more = ruleOut(desktopOut, ".nd-root .nd-chrome", "@media (prefers-contrast:more)");
    expect(more).toContain("background:var(--surface2)");
    expect(more).toMatch(/outline:1px solid var\(--edge-strong\)/);
  });

  it("nenhum CSS da interface perde a forma padrão de propriedades com variante prefixada", () => {
    // Propriedades que o lightningcss junta com a variante -webkit-/-moz-: a padrão tem de sair no build.
    const PROPS = ["backdrop-filter", "user-select", "appearance", "mask", "mask-image", "text-size-adjust", "background-clip", "box-decoration-break", "hyphens", "clip-path"];
    const lost: string[] = [];
    for (const { file, css } of files) {
      const source = css.replace(/\/\*[\s\S]*?\*\//g, "");
      const out = buildCss(css, file);
      for (const prop of PROPS) {
        const declared = source.match(new RegExp(`(?<![-\\w])${prop}\\s*:`, "g"))?.length ?? 0;
        if (declared === 0) continue;
        const kept = out.match(new RegExp(`(?<![-\\w])${prop}:`, "g"))?.length ?? 0;
        if (kept < declared) lost.push(`${file}: ${prop} (${declared} na fonte, ${kept} no build)`);
      }
    }
    expect(lost).toEqual([]);
  });

  it("na fonte, a forma prefixada vem sempre ANTES da padrão (a última declarada é a que o lightningcss mantém)", () => {
    const offenders: string[] = [];
    for (const { file, css } of files) {
      for (const m of css.matchAll(/(?<![-\w])(backdrop-filter|user-select|appearance|mask)\s*:[^;]+;\s*-(?:webkit|moz)-\1\s*:/g)) offenders.push(`${file}: ${m[0]}`);
    }
    expect(offenders).toEqual([]);
  });

  it("a sequência de teste reproduz o defeito: com a prefixada por último, a padrão some", () => {
    const out = buildCss(".x{backdrop-filter:blur(2px);-webkit-backdrop-filter:blur(2px)}", "x.css");
    expect(out).not.toMatch(/(?:\{|;)backdrop-filter:/);
  });
});
