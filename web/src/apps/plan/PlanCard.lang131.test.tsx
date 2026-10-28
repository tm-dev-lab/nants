/*
 * Spec 131 T06: em en, o "Mudou nesta versão" monta as linhas pelo código (e mostra as antigas, em
 * texto, como vieram) e o papel das orientações (título gravado em pt-BR) sai no idioma ativo.
 */
import { cleanup, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { PlanChangeEntry, SpecView } from "../../api/types";
import { setLanguage } from "../../i18n";
import { makeSpec, makeTask, renderWith, resetStore } from "../../test-utils";
import { PlanCard } from "./PlanCard";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
});

/** As linhas do Test 9, agora com código, mais uma gravada antes (texto). */
const CHANGES: PlanChangeEntry[] = [
  { code: "task_depends", params: { id: "T07" } },
  { code: "task_scope", params: { id: "T07" } },
  { code: "task_criteria", params: { id: "T07", added: 0, removed: 1 } },
  { code: "task_new", params: { id: "T06", title: "Provide executable launcher with CLI parity" } },
  { code: "global_criteria", params: { added: 2, removed: 0 } },
  { code: "desconhecido_x", params: {} },
  "T02 removida",
];

function spec(): SpecView {
  return {
    ...makeSpec("s1", "Household ledger", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
    role_guidance: [
      { role: "Desenvolvedor", text: "Use integer cents." },
      { role: "Revisor", text: "Check overlaps." },
    ],
    plan_changes: CHANGES,
  };
}

function open(lang: "en" | "pt-BR"): HTMLElement {
  resetStore({});
  renderWith(<PlanCard projectId="p1" spec={spec()} stage="awaiting" onApproved={async () => {}} />, { lang });
  const card = document.querySelector<HTMLElement>("[data-plan]");
  if (card === null) throw new Error("plano ausente");
  for (const details of card.querySelectorAll("details")) details.open = true;
  return card;
}

describe("PlanCard no idioma do projeto (spec 131 T06)", () => {
  it("em en: linhas das mudanças pelo código e títulos dos papéis traduzidos", () => {
    const card = open("en");
    const region = within(card).getByRole("region", { name: "Changed in this version" });
    const items = within(region)
      .getAllByRole("listitem")
      .map((li) => li.textContent);
    expect(items).toEqual([
      "T07: dependencies changed",
      "T07: scope changed",
      "T07: criteria changed (1 removed)",
      "T06 new: Provide executable launcher with CLI parity",
      "Global criteria: 2 new",
      "T02 removida",
    ]);
    expect(region.textContent).toContain("(6)");
    const guidance = card.querySelector<HTMLElement>("[data-plan-guidance]");
    if (guidance === null) throw new Error("orientações ausentes");
    const roles = [...guidance.querySelectorAll("h4")].map((h) => h.textContent);
    expect(roles).toEqual(["Developer", "Reviewer"]);
  });

  it("em pt-BR: as mesmas linhas no texto de antes", () => {
    const card = open("pt-BR");
    const region = within(card).getByRole("region", { name: "Mudou nesta versão" });
    const items = within(region)
      .getAllByRole("listitem")
      .map((li) => li.textContent);
    expect(items.slice(0, 5)).toEqual([
      "T07: dependências alteradas",
      "T07: escopo alterado",
      "T07: critérios alterados (1 removido)",
      "T06 nova: Provide executable launcher with CLI parity",
      "Critérios globais: 2 novos",
    ]);
    const roles = [...card.querySelectorAll("[data-plan-guidance] h4")].map((h) => h.textContent);
    expect(roles).toEqual(["Desenvolvedor", "Revisor"]);
  });
});
