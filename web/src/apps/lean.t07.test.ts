/*
 * Regras de CSS da auditoria visual da 095/T07 (A-04, A-05, A-06): sem caixa contornada dentro de
 * outra caixa. O jsdom não calcula layout: aqui se conferem as regras e a estrutura; a captura real
 * fica com o Arquiteto.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { cssRuleBody, cssRules } from "../desktop/test-utils";

const SRC = resolve(process.cwd(), "src");
const read = (path: string): string => readFileSync(resolve(SRC, path), "utf8");
const appsCss = read("apps/shared/apps.css");
const reportCss = read("apps/report/report.css");

/** Bordas que desenham algo (não `0`/`none`), fora do raio. */
const drawnBorders = (body: string): string[] =>
  [...body.matchAll(/(?:^|;)\s*(border(?:-(?!radius)[a-z]+)*)\s*:\s*([^;]+);/g)]
    .filter((m) => !/^(?:0|none)$/.test((m[2] ?? "").trim()))
    .map((m) => `${m[1]}: ${(m[2] ?? "").trim()}`);

describe("A-04: \"Suas escolhas\" é uma divisória com rótulo, sem caixa", () => {
  it("só a divisória de cima, sem fundo nem contorno", () => {
    const body = cssRuleBody(appsCss, ".nd-choices-answered");
    expect(drawnBorders(body)).toEqual(["border-top: 1px solid var(--divider)"]);
    expect(body).not.toMatch(/background|box-shadow/);
  });

  it("o componente não usa cartão", () => {
    const tsx = read("apps/conversation/ChoicesForm.tsx");
    expect(tsx).toMatch(/className="nd-choices-answered"/);
    expect(tsx).not.toMatch(/nd-card/);
  });
});

describe("A-05: blocos de código do relatório só com fundo", () => {
  it("no Relatório o <pre> não tem contorno; o fundo continua", () => {
    expect(cssRuleBody(reportCss, ".nd-report .nd-md-pre")).toContain("border: 0;");
    expect(cssRuleBody(appsCss, ".nd-md-pre")).toContain("background: var(--surface2);");
  });
});

describe("nota da revisão da T06: sem a classe órfã nd-project-danger", () => {
  it("nenhum componente nem CSS usa a classe cuja regra foi apagada", () => {
    expect(read("apps/project/ProjectApp.tsx")).not.toContain("nd-project-danger");
    expect(read("apps/project/project.css")).not.toContain("nd-project-danger");
  });
});

describe("A-06: item do sino é uma linha com --divider, não um cartão", () => {
  it("linha com a divisória de baixo, sem contorno, fundo ou cartão", () => {
    const body = cssRuleBody(appsCss, ".nd-attention-item");
    expect(drawnBorders(body)).toEqual(["border-bottom: 1px solid var(--divider)"]);
    expect(body).not.toMatch(/background|box-shadow|border-radius/);
    // Nenhuma regra devolve contorno a um tipo de item (a de permissão tinha borda âmbar).
    const typed = cssRules(appsCss).filter((r) => r.selectors.some((s) => s.startsWith(".nd-attention-item[")));
    expect(typed.filter((r) => drawnBorders(r.body).length > 0 || /border-color/.test(r.body))).toEqual([]);
    const tsx = read("apps/attention/AttentionPanel.tsx");
    expect(tsx).not.toMatch(/nd-card/);
  });
});
