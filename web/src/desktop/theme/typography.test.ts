// @vitest-environment node
/*
 * Tipografia da 095/T02 (decisão 3; C-08, C-09 e C-15). O jsdom não aplica CSS: a escala vem dos
 * tokens e as regras, do CSS-fonte.
 * - Escala: micro 10, caption 11, label 12, control 13, body 14, heading 17, display 20, numeral 32 e
 *   mono 12, mais `--lh-body` 1.55 e `--ls-tight` -0.006em, aplicados na `.nd-root`.
 * - Peso 700 fora de uso (a Inter importada vai até o 600).
 * - Monoespaçada SÓ nas categorias da C-08: id de tarefa, código (em linha e bloco), console e
 *   registro, caminhos de arquivo e hashes de commit. `<kbd>`, endereço, custos, horas e metadados
 *   são sem serifa (os custos com `tabular-nums`, classe `.nd-num`).
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../test-utils";
import { FONT_SIZES, FONT_WEIGHTS, LETTER_SPACING_TIGHT, LINE_HEIGHT_BODY, scaleVariables, THEME_NAMES, themeVariables } from "./tokens";

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");
const desktopCss = read("desktop/theme/desktop.css");
const appsCss = read("apps/shared/apps.css");

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
const allCss = (): { file: string; css: string }[] => walk(SRC, /\.css$/).map((full) => ({ file: rel(full), css: readFileSync(full, "utf8") }));
const sources = (): { file: string; text: string }[] =>
  walk(SRC, /\.tsx?$/)
    .filter((f) => !/\.test\.tsx?$/.test(f) && !f.endsWith("test-utils.tsx") && !f.endsWith("test-utils.ts"))
    .map((full) => ({ file: rel(full), text: readFileSync(full, "utf8") }));

describe("escala tipográfica (C-09)", () => {
  it("os tamanhos por papel são os do mockup", () => {
    expect(FONT_SIZES).toMatchObject({
      micro: "10px",
      caption: "11px",
      label: "12px",
      control: "13px",
      body: "14px",
      heading: "17px",
      display: "20px",
      numeral: "32px",
      mono: "12px",
    });
  });

  it("entrelinha do corpo e espaçamento entre letras como variáveis da escala, nos dois temas", () => {
    expect(LINE_HEIGHT_BODY).toBe(1.55);
    expect(LETTER_SPACING_TIGHT).toBe("-0.006em");
    expect(scaleVariables()["--lh-body"]).toBe("1.55");
    expect(scaleVariables()["--ls-tight"]).toBe("-0.006em");
    for (const theme of THEME_NAMES) {
      expect(themeVariables(theme)["--lh-body"]).toBe("1.55");
      expect(themeVariables(theme)["--ls-tight"]).toBe("-0.006em");
    }
  });

  it("a .nd-root aplica o corpo de 14 px, a entrelinha e o espaçamento", () => {
    const root = cssRuleBody(desktopCss, ".nd-root");
    expect(root).toContain("font-size: var(--fs-body);");
    expect(root).toContain("line-height: var(--lh-body);");
    expect(root).toContain("letter-spacing: var(--ls-tight);");
  });

  it("nenhum papel que saiu da escala (o título virou `control`) é usado", () => {
    for (const { file, css } of allCss()) expect({ file, old: /--fs-title\b/.test(css) }).toEqual({ file, old: false });
  });

  // A-02 da 094, atualizada para a escala da 095: metadados em `label` (12), sem serifa e `--muted`.
  it("A-02: a linha de metadados usa o papel label (12 px)", () => {
    const meta = cssRuleBody(appsCss, ".nd-meta-line");
    expect(meta).toContain("font-size: var(--fs-label);");
    expect(meta).toContain("font-family: var(--font-sans);");
  });
});

describe("peso 700 fora de uso (C-15)", () => {
  it("FONT_WEIGHTS vai até o 600 (a Inter importada em main.tsx não tem o 700)", () => {
    expect(Object.values(FONT_WEIGHTS)).toEqual([400, 500, 600]);
    expect(read("main.tsx")).not.toMatch(/inter\/latin-700/);
  });

  it("o negrito padrão do navegador (strong, b, títulos, th) vira o 600", () => {
    for (const tag of ["b", "strong", "th", "h1", "h2", "h3", "h4", "h5", "h6"]) {
      expect(cssRuleBody(desktopCss, `:where(.nd-root) ${tag}`)).toContain("font-weight: var(--fw-semibold);");
    }
  });

  it("nenhum CSS usa --fw-bold, 700 ou bold", () => {
    for (const { file, css } of allCss()) {
      const clean = css.replace(/\/\*[\s\S]*?\*\//g, "");
      expect({ file, bold: /--fw-bold|font-weight:\s*(?:700|bold)|font:\s*(?:700|bold)\b/.test(clean) }).toEqual({ file, bold: false });
    }
  });
});

/**
 * Seletores que podem declarar a fonte mono (`--font-mono` ou `--fs-mono`), cada um com a categoria
 * da C-08. Lista FECHADA: um seletor novo com mono precisa entrar aqui com o motivo.
 */
const MONO_SELECTORS: ReadonlyMap<string, string> = new Map([
  [".nd-mono", "classe utilitária; os arquivos que a usam estão em MONO_FILES, cada um com a categoria"],
  [".nd-input.nd-mono", "campos de caminho de pasta (Projetos, Novo projeto): caminhos de arquivo"],
  [".nd-md-pre", "bloco de código no markdown"],
  [".nd-md-code", "código em linha no markdown"],
  [".nd-app-log", "console da execução do app"],
  [".nd-plan-task-id", "id de tarefa no Plano"],
  [".nd-nmenu-version", "versão do Nants no topo do menu do N (identificador de versão, como um hash; spec 137, pedido do usuário)"],
  [".nd-kb-src-doc-id", "id do documento que saiu da base, no quadro Fontes da base do Plano (identificador, spec 136 T04)"],
]);

/** Arquivos `.tsx` que podem usar `nd-mono`, com o que é mono em cada um (categorias da C-08). */
const MONO_FILES: ReadonlyMap<string, string> = new Map([
  ["apps/activity/ActivityApp.tsx", "id de tarefa e comando do registro de ferramentas"],
  ["apps/app/ConsolePanel.tsx", "comando digitado no Console, a saída dele e o histórico de comandos (spec 130)"],
  ["apps/app/RunViews.tsx", "comando, porta, saída do programa, registro e caminhos dos arquivos criados (spec 130)"],
  ["apps/execution/ExecutionApp.tsx", "comando de execução, caminho do projeto, detalhes da falha e console"],
  ["apps/attention/AttentionPanel.tsx", "comando pedindo permissão (código)"],
  ["apps/attention/PermissionToasts.tsx", "comando pedindo permissão (código)"],
  ["apps/board/BoardApp.tsx", "id de tarefa no cartão"],
  ["apps/board/ToolActionsSheet.tsx", "comando no registro de ações"],
  ["apps/conversation/Bubble.tsx", "comando liberado pela aprovação automática (código) e id de tarefa"],
  ["apps/conversation/PausedRunCard.tsx", "comando, ids de tarefa"],
  ["apps/conversation/PermissionMessage.tsx", "comando, diff e id de tarefa"],
  ["apps/conversation/ResultMessage.tsx", "comando de teste, caminhos de arquivo, id de tarefa e hash de commit"],
  ["apps/files/FileViewer.tsx", "caminho do arquivo, diff e código"],
  ["apps/plan/PlanCard.tsx", "ids de tarefa e caminhos dos arquivos de ambiente"],
  ["apps/project/ProjectApp.tsx", "caminho do projeto"],
  ["apps/project/RemoveProjectDialog.tsx", "caminho do projeto"],
  ["apps/settings/MachineTab.tsx", "comando de correção (código)"],
  ["apps/settings/NetworkSection.tsx", "script do PowerShell para o WSL (código)"],
  ["apps/settings/ProjectsRootTab.tsx", "caminho da pasta de projetos"],
  ["apps/setup/SetupApp.tsx", "comando de correção (código)"],
  ["apps/shared/ActivityBubble.tsx", "comando, linha do registro de ferramentas e id de tarefa"],
  ["apps/shared/CaptureThumbnails.tsx", "nome do arquivo da captura (caminho)"],
  ["apps/shared/ImageLightbox.tsx", "nome do arquivo da imagem (caminho)"],
  ["apps/shared/ReviewMessage.tsx", "local do apontamento (caminho e linha)"],
  ["desktop/NewProjectDialog.tsx", "caminhos e nomes de pasta"],
]);

describe("mono só nas categorias da C-08", () => {
  it("toda regra CSS com --font-mono ou --fs-mono tem o seletor na lista fechada", () => {
    const offenders: string[] = [];
    for (const { file, css } of allCss()) {
      if (file === "styles.css") continue; // `@theme` do Tailwind: só define a família.
      for (const rule of cssRules(css)) {
        if (!/--font-mono|--fs-mono/.test(rule.body)) continue;
        for (const selector of rule.selectors) if (!MONO_SELECTORS.has(selector)) offenders.push(`${file}: ${selector}`);
      }
    }
    expect(offenders).toEqual([]);
    for (const [, reason] of MONO_SELECTORS) expect(reason.length).toBeGreaterThan(10);
  });

  it("a checagem pega um seletor novo com mono", () => {
    const sample = ".nd-time { font: var(--fs-mono) var(--font-mono); } .nd-mono { font-family: var(--font-mono); }";
    const hits = cssRules(sample).filter((r) => /--font-mono|--fs-mono/.test(r.body) && r.selectors.some((s) => !MONO_SELECTORS.has(s)));
    expect(hits.map((r) => r.selectors[0])).toEqual([".nd-time"]);
  });

  it("`nd-mono` só nos arquivos da lista de exceções (cada um com o motivo)", () => {
    const offenders: string[] = [];
    const used = new Set<string>();
    for (const { file, text } of sources()) {
      if (!/\bnd-mono\b/.test(text)) continue;
      used.add(file);
      if (!MONO_FILES.has(file)) offenders.push(file);
    }
    expect(offenders).toEqual([]);
    // A lista não guarda arquivo que já não usa mono.
    expect([...MONO_FILES.keys()].filter((f) => !used.has(f))).toEqual([]);
    for (const [, reason] of MONO_FILES) expect(reason.length).toBeGreaterThan(10);
  });

  it("<kbd>, endereço, custos, horas e metadados são sem serifa", () => {
    const kbd = cssRuleBody(desktopCss, ".nd-root kbd");
    expect(kbd).toContain("font-family: var(--font-sans);");
    expect(cssRuleBody(read("apps/app/app.css"), ".nd-app-address")).not.toMatch(/--fs-mono|--font-mono/);
    expect(cssRuleBody(desktopCss, ".nd-num")).toContain("font-variant-numeric: tabular-nums;");
    const costs = sources().find((s) => s.file === "apps/costs/CostsApp.tsx")?.text ?? "";
    expect(costs).not.toMatch(/\bnd-mono\b/);
    expect(costs).toMatch(/\bnd-num\b/);
    // Horas (`<time>`) nunca em mono.
    for (const { file, text } of sources()) {
      for (const m of text.matchAll(/<time\b[^>]*className="([^"]*)"/g)) expect({ file, cls: m[1] }).toEqual({ file, cls: expect.not.stringMatching(/\bnd-mono\b/) });
    }
  });
});
