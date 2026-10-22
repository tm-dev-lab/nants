/*
 * Regras de CSS da 095/T06 (Configurações, Arquivos, Relatório, Projeto e Primeiros passos
 * enxutos, e o celular). O jsdom não calcula layout: aqui se conferem as regras; a captura real a
 * 390 px fica com a T07.
 */
import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../desktop/test-utils";

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");

const T06_CSS = ["apps/settings/settings.css", "apps/files/files.css", "apps/report/report.css", "apps/project/project.css"] as const;
const shellCss = read("desktop/theme/shell.css");
const settingsCss = read("apps/settings/settings.css");

/** Todos os .css sob `src/`. */
function allCss(): { file: string; text: string }[] {
  return readdirSync(SRC, { recursive: true, encoding: "utf8" })
    .filter((name) => name.endsWith(".css"))
    .map((name) => ({ file: name.split("\\").join("/"), text: readFileSync(join(SRC, name), "utf8") }));
}

/** Largura útil de uma vista de tela cheia a 390 px, descontada a margem lateral (2 × 16). */
const NARROW_BUDGET_PX = 390 - 32;

/** Valores em px de uma declaração, fora de `min(...)` (onde a regra já limita a 100%). */
function loosePx(value: string): number[] {
  const withoutMin = value.replace(/min\([^()]*\)/g, "");
  return [...withoutMin.matchAll(/(\d+(?:\.\d+)?)px/g)].map((m) => Number(m[1]));
}

describe("folha de atalhos: `nd-sheet` tem um dono só (observação da revisão da T01)", () => {
  it("só a folha de atalhos define `.nd-sheet`; a folha de Ações do Quadro é `.nd-actions-sheet`", () => {
    const owners = allCss()
      .filter(({ text }) => cssRules(text).some((r) => r.selectors.some((s) => /\.nd-sheet(?![\w-])/.test(s))))
      .map(({ file }) => file);
    expect(owners).toEqual(["desktop/theme/desktop.css"]);
    expect(read("apps/board/ToolActionsSheet.tsx")).not.toMatch(/"nd-sheet[\s"]/);
    expect(cssRuleBody(read("apps/board/board.css"), ".nd-actions-sheet")).toContain("position: absolute;");
  });
});

describe("janelas enxutas: divisórias e faixas no lugar de caixas contornadas", () => {
  /** Controles (não estrutura) que mantêm o contorno, com o motivo. */
  const CONTROL_EDGES = new Map([[".nd-remove-volumes", "opção marcável do diálogo de remoção (controle, como o `nd-radio-card`)"]]);

  it.each(T06_CSS)("%s: nenhum contorno `--line`/`--line2` na estrutura (ela usa `--divider`)", (file) => {
    const offenders = cssRules(read(file))
      .filter((r) => !r.selectors.every((s) => CONTROL_EDGES.has(s)))
      .filter((r) => /border(?:-\w+)?:\s*1px solid var\(--line2?\)/.test(r.body))
      .map((r) => r.selectors.join(", "));
    expect(offenders).toEqual([]);
  });

  it("linhas de lista com `--divider`: conexão, modelo, pré-requisito, papel e o que falta na máquina", () => {
    for (const selector of [".nd-conn", ".nd-price-card, .nd-prereq"]) {
      const rule = cssRules(settingsCss).find((r) => r.selectors.join(", ") === selector);
      expect(rule?.body, selector).toContain("border-bottom: 1px solid var(--divider);");
    }
    expect(cssRuleBody(shellCss, ".nd-role-card")).toContain("border-bottom: 1px solid var(--divider);");
    expect(cssRuleBody(shellCss, ".nd-missing")).toContain("border-bottom: 1px solid var(--divider);");
    expect(cssRuleBody(read("apps/project/project.css"), ".nd-project-section")).toContain("border-bottom: 1px solid var(--divider);");
    expect(cssRuleBody(read("apps/report/report.css"), ".nd-completed")).toContain("background: var(--lane);");
  });

  it("os componentes das janelas da T06 não usam `nd-card` (sem caixa dentro de caixa)", () => {
    for (const file of [
      "apps/settings/SettingsApp.tsx",
      "apps/settings/ConnectionRow.tsx",
      "apps/settings/AgentsForm.tsx",
      "apps/settings/PricingEditor.tsx",
      "apps/settings/MachineTab.tsx",
      "apps/settings/ProjectsRootTab.tsx",
      "apps/files/FilesApp.tsx",
      "apps/report/ReportApp.tsx",
      "apps/project/ProjectApp.tsx",
      "apps/setup/SetupApp.tsx",
    ]) {
      expect(read(file), file).not.toMatch(/className="[^"]*\bnd-card\b/);
    }
  });

  it("raio moderado: chips e selos com texto usam `--r-md`, nunca `--r-pill` (decisão 9)", () => {
    expect(cssRuleBody(settingsCss, ".nd-chip-static")).toContain("border-radius: var(--r-md);");
    expect(cssRuleBody(shellCss, ".nd-tag")).toContain("border-radius: var(--r-md);");
    expect(cssRuleBody(shellCss, ".nd-step")).toContain("border-radius: var(--r-md);");
    expect(cssRuleBody(settingsCss, ".nd-settings-tab")).toContain("border-radius: var(--r-md);");
  });
});

describe("celular: nada rola na horizontal a 390 px", () => {
  it.each(T06_CSS)("%s: nenhuma largura fixa maior que a vista (fora de `min()` e `max-width`)", (file) => {
    const offenders: string[] = [];
    for (const rule of cssRules(read(file))) {
      for (const decl of rule.body.split(";")) {
        const [prop, ...rest] = decl.split(":");
        const name = (prop ?? "").trim();
        if (!/^(width|min-width|flex-basis|flex|grid-template-columns)$/.test(name)) continue;
        if (loosePx(rest.join(":")).some((px) => px > NARROW_BUDGET_PX)) offenders.push(`${rule.selectors.join(", ")} { ${decl.trim()} }`);
      }
    }
    expect(offenders).toEqual([]);
  });

  it("grades auto-fit encolhem até 100% (`minmax(min(Npx, 100%), 1fr)`)", () => {
    const grids = [cssRuleBody(settingsCss, ".nd-price-grid"), cssRuleBody(shellCss, ".nd-role-card"), cssRuleBody(shellCss, ".nd-radio-grid")];
    for (const body of grids) expect(body).toMatch(/repeat\(auto-fit, minmax\(min\(\d+px, 100%\), 1fr\)\)/);
  });

  it("estreita: Conexões põem 'usada por' sob o detalhe; os limites do Projeto empilham", () => {
    expect(cssRuleBody(settingsCss, '.nd-settings[data-size="narrow"] .nd-conn-row')).toContain("grid-template-columns: auto minmax(0, 1fr) auto;");
    expect(cssRuleBody(settingsCss, '.nd-settings[data-size="narrow"] .nd-conn-usage')).toContain("white-space: normal;");
    expect(cssRuleBody(settingsCss, ".nd-conn-detail")).toContain("text-overflow: ellipsis;");
    expect(cssRuleBody(read("apps/project/project.css"), '.nd-project[data-size="narrow"] .nd-project-limits')).toContain(
      "grid-template-columns: minmax(0, 1fr);",
    );
  });

  it("abas das Configurações e ferramentas dos Arquivos numa linha só, rolável dentro do slot (C-07)", () => {
    const tabs = cssRuleBody(settingsCss, ".nd-settings-tabs");
    expect(tabs).toContain("overflow-x: auto;");
    expect(tabs).toContain("min-width: 0;");
    const files = read("apps/files/files.css");
    expect(cssRuleBody(files, ".nd-files-segment")).toContain("flex-wrap: nowrap;");
    expect(cssRuleBody(files, ".nd-segment-btn")).toContain("text-overflow: ellipsis;");
    expect(cssRuleBody(read("desktop/theme/mobile.css"), '.nd-root[data-mobile="true"] .nd-window-tools')).toContain("overflow-x: auto;");
  });
});
