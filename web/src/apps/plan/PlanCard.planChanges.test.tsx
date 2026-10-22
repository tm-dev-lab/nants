/* Janela Plano (spec 093, T06). */
/*
 * "Mudou nesta versão" no PlanCard (spec 086/T09; 095/T04): as linhas do servidor ficam no
 * `<details>` recolhido do plano (aberto pelo `summary` no teste, C-11) e somem sem mudanças.
 */
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { PlanCard } from "./PlanCard";
import { makeSpec, makeTask, resetStore } from "../../test-utils";

afterEach(cleanup);

function renderWith(changes: string[] | undefined) {
  resetStore({});
  const spec = {
    ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
    ...(changes !== undefined ? { plan_changes: changes } : {}),
  };
  render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
}

const LINES = ["T04 nova: Tela de login", "T02 removida", "T03: critérios alterados (2 novos, 1 removido)"];

describe("PlanCard: mudou nesta versão", () => {
  it("sem mudanças, não mostra o quadro", () => {
    renderWith(undefined);
    expect(screen.queryByRole("region", { name: "Mudou nesta versão" })).toBeNull();
    cleanup();
    renderWith([]);
    expect(screen.queryByText(/Mudou nesta versão/)).toBeNull();
  });

  it("fica no detalhe recolhido: o summary abre e mostra as linhas", () => {
    renderWith(LINES);
    const details = document.querySelector<HTMLDetailsElement>("[data-plan-more]");
    if (details === null) throw new Error("detalhes ausentes");
    expect(details.open).toBe(false);
    const summary = details.querySelector("summary");
    expect(summary?.textContent).toBe("Checagens e o que mudou");
    if (summary !== null) fireEvent.click(summary);
    details.open = true;
    const region = within(details).getByRole("region", { name: "Mudou nesta versão" });
    for (const line of LINES) expect(within(region).getByText(line)).toBeTruthy();
    expect(region.textContent).toContain("(3)");
  });
});
