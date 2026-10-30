// @vitest-environment node
/*
 * Arredondamento moderado (spec 095, decisão 9; pedido do usuário em 2026-10-20). Controles com
 * texto (a pílula de execução, as pílulas de estado, chips, tags, avisos e o Parar da pílula) usam o
 * raio dos controles (`--r-md`/`--r-lg`). `--r-pill` fica só para pontos de estado, selos de
 * contagem, o trilho do interruptor e as barras de progresso: a lista abaixo é FECHADA. Um seletor
 * novo com `--r-pill` faz o teste falhar até alguém decidir, aqui, a categoria dele.
 */
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve, sep } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../test-utils";

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

type Category = "ponto" | "selo de contagem" | "interruptor" | "progresso";

/** Lista fechada: seletor → categoria da decisão 9. */
const ALLOWED: ReadonlyMap<string, Category> = new Map<string, Category>([
  [".nd-dot", "ponto"],
  [".nd-agent-dot", "ponto"],
  [".nd-exec-dot", "ponto"],
  [".nd-tree-dot", "ponto"],
  // Menu do N (spec 137): o ponto rosa de novidade no botão N e o ponto da janela aberta na grade.
  [".nd-nbtn-dot", "ponto"],
  [".nd-nmenu-indicator", "ponto"],
  // Avatar do agente e o anel dele: o ponto redondo que identifica o papel (sem texto).
  [".nd-avatar", "ponto"],
  [".nd-avatar-ring", "ponto"],
  // Marca numerada do passo dos Primeiros passos: um ponto com o número (20 px, sem texto corrido).
  [".nd-step-mark", "ponto"],
  [".nd-badge", "selo de contagem"],
  [".nd-jump-count", "selo de contagem"],
  [".nd-settings-tab-count", "selo de contagem"],
  [".nd-switch", "interruptor"],
  [".nd-switch::after", "interruptor"],
  [".nd-app-progress", "progresso"],
  [".nd-app-progress::after", "progresso"],
  [".nd-costs-budget-track", "progresso"],
  [".nd-costs-budget-fill", "progresso"],
  [".nd-cost-bar-track", "progresso"],
  [".nd-cost-bar-fill", "progresso"],
]);

const sheets = walk(SRC).map((full) => ({ file: relative(SRC, full).split(sep).join("/"), css: readFileSync(full, "utf8") }));

describe("--r-pill só na lista fechada (decisão 9)", () => {
  it("nenhum seletor fora da lista usa --r-pill", () => {
    const offenders: string[] = [];
    const seen = new Set<string>();
    for (const { file, css } of sheets) {
      for (const rule of cssRules(css)) {
        if (!/border-radius:[^;]*var\(--r-pill\)/.test(rule.body)) continue;
        for (const selector of rule.selectors) {
          seen.add(selector);
          if (!ALLOWED.has(selector)) offenders.push(`${file}: ${selector}`);
        }
      }
    }
    expect(offenders).toEqual([]);
    // A lista não guarda seletores mortos: todo item dela ainda existe com `--r-pill`.
    expect([...ALLOWED.keys()].filter((s) => !seen.has(s))).toEqual([]);
  });

  it("a checagem pega uma pílula de texto nova", () => {
    const sample = ".nd-chip-x { padding: 0 var(--sp-2); border-radius: var(--r-pill); } .nd-dot { border-radius: var(--r-pill); }";
    const hits = cssRules(sample)
      .filter((r) => /border-radius:[^;]*var\(--r-pill\)/.test(r.body))
      .flatMap((r) => r.selectors)
      .filter((s) => !ALLOWED.has(s));
    expect(hits).toEqual([".nd-chip-x"]);
  });

  const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");
  it.each([
    ["pílula de execução", "desktop/theme/shell.css", ".nd-run-pill", "var(--r-lg)"],
    ["Parar/Retomar da pílula (r-lg − sp-1)", "desktop/theme/desktop.css", ".nd-pill-btn", "var(--r-md)"],
    ["pílulas de estado e selos (veredito, severidade, Parada/Subindo/No ar)", "apps/shared/apps.css", ".nd-pill", "var(--r-md)"],
    ["tag", "desktop/theme/shell.css", ".nd-tag", "var(--r-md)"],
    ["chip", "apps/settings/settings.css", ".nd-chip-static", "var(--r-md)"],
    ["aviso flutuante", "desktop/theme/shell.css", ".nd-toast", "var(--r-lg)"],
    ["botão do aviso (r-lg − sp-1)", "desktop/theme/shell.css", ".nd-toast .nd-btn", "var(--r-md)"],
    ["dica", "desktop/theme/desktop.css", ".nd-hint", "var(--r-lg)"],
    ["aviso sob a barra", "desktop/theme/shell.css", ".nd-bar-alert", "var(--r-lg)"],
    ["aviso de permissão", "apps/shared/apps.css", ".nd-permission-toast", "var(--r-lg)"],
    ["mais avisos", "apps/shared/apps.css", ".nd-toast-more", "var(--r-md)"],
  ])("%s: raio de controle", (_label, file, selector, radius) => {
    expect(cssRuleBody(read(file), selector)).toContain(`border-radius: ${radius};`);
  });
});
