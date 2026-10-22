/*
 * Plano enxuto (095/T04, `reference/Janelas.dc.html`): título e resumo, tarefas em lista (id mono,
 * título, meta à direita), faixa âmbar das checagens, `<details>` recolhido e rodapé com "Aprovar e
 * iniciar" (a única primária) e "Pedir correção"; as etapas da 094/T03 no rodapé.
 */
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { SpecView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { PLAN_DRAFTING_NOTICE, PLAN_REVIEW_WAITING } from "../../copy";
import type { PlanStage } from "../../phase";
import { makeSpec, makeTask, resetStore } from "../../test-utils";
import { PlanCard } from "./PlanCard";

afterEach(cleanup);

function spec(extra: Partial<SpecView> = {}): SpecView {
  return {
    ...makeSpec("s1", "Sistema Solar", "AWAITING_APPROVAL", [makeTask("T01", "Base Vite", "TODO"), makeTask("T02", "Motor orbital", "TODO", { depends_on: ["T01"] })]),
    plan_review: { status: "ok", issues: [{ task: "T02", kind: "untestable", text: "T02 não define como verificar a escala." }] },
    premises: ["Sem backend"],
    ...extra,
  };
}

function view(s: SpecView, stage?: PlanStage | null) {
  resetStore({});
  return render(<PlanCard projectId="p1" spec={s} onApproved={async () => {}} onRequestFix={() => {}} {...(stage !== undefined ? { stage } : {})} />);
}

describe("Plano enxuto", () => {
  it("ordem: cabeçalho, tarefas, faixa âmbar, detalhe recolhido e rodapé", () => {
    const { container } = view(spec());
    const plan = container.querySelector("[data-plan]");
    if (plan === null) throw new Error("plano ausente");
    const order = Array.from(plan.children).map((el) => el.className.split(" ")[0]);
    expect(order).toEqual(["nd-plan-header", "nd-plan-tasks", "nd-plan-warn", "nd-plan-more", "nd-plan-foot"]);
    expect(within(plan.querySelector(".nd-plan-header") as HTMLElement).getByRole("heading", { level: 2 }).textContent).toBe("Sistema Solar");
    expect(plan.querySelector(".nd-plan-summary")?.textContent).toBe("s1 · 2 tarefas");
    expect(plan.querySelector<HTMLDetailsElement>("[data-plan-more]")?.open).toBe(false);
    expect(plan.querySelector("[data-plan-more] > summary")?.textContent).toBe("Premissas e checagens");
    expect(findForbiddenGlyphs(plan as HTMLElement)).toEqual([]);
  });

  it("tarefa: id mono, título e 'depende de' à direita em sans", () => {
    view(spec());
    const items = within(screen.getByRole("list", { name: "Tarefas do plano" })).getAllByRole("listitem");
    const second = items[1];
    if (second === undefined) throw new Error("tarefa ausente");
    expect(second.querySelector(".nd-plan-task-id.nd-mono")?.textContent).toBe("T02");
    expect(second.querySelector(".nd-plan-task-title")?.textContent).toBe("Motor orbital");
    const meta = second.querySelector(".nd-plan-task-meta");
    expect(meta?.textContent).toBe("depende de T01");
    expect(meta?.querySelector(".nd-mono")).toBeNull();
  });

  it("faixa âmbar com o título e o texto do Revisor", () => {
    view(spec());
    const warn = screen.getByRole("region", { name: "Checagens do plano" });
    expect(within(warn).getByRole("heading", { name: "O Revisor fez 1 apontamento" })).toBeTruthy();
    expect(warn.textContent).toContain("T02 não define como verificar a escala.");
  });

  it("rodapé: 'Aprovar e iniciar' é a única primária, com 'Pedir correção' secundária ao lado", () => {
    const { container } = view(spec(), "awaiting");
    const foot = container.querySelector(".nd-plan-foot");
    if (foot === null) throw new Error("rodapé ausente");
    const buttons = within(foot as HTMLElement).getAllByRole("button");
    expect(buttons.map((b) => b.textContent)).toEqual(["Aprovar e iniciar", "Pedir correção"]);
    expect(container.querySelectorAll('[data-variant="primary"]')).toHaveLength(1);
    expect(buttons[1]?.getAttribute("data-variant")).toBeNull();
  });

  it("etapas da 094/T03 no rodapé: planejando sem ações, revisando desabilitado, aguardando liberado", () => {
    const { unmount } = view(spec({ plan_review: { status: "running", issues: [] } }), "planning");
    expect(screen.queryByRole("button", { name: "Aprovar e iniciar" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Checagens do plano" })).toBeNull();
    expect(screen.getAllByText(PLAN_DRAFTING_NOTICE).length).toBeGreaterThan(0);
    unmount();

    const reviewing = view(spec({ plan_review: { status: "running", issues: [] }, scope_warnings: ["T01: cita deps.edn"] }), "reviewing");
    expect((screen.getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Pedir correção" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(PLAN_REVIEW_WAITING)).toBeTruthy();
    reviewing.unmount();

    view(spec(), "awaiting");
    expect((screen.getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement).disabled).toBe(false);
    expect((screen.getByRole("button", { name: "Pedir correção" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("fora da aprovação: rodapé com a situação e sem 'Aprovar'", () => {
    view(spec({ status: "IN_PROGRESS", plan_review: { status: "ok", issues: [] } }), null);
    expect(screen.queryByRole("button", { name: "Aprovar e iniciar" })).toBeNull();
    expect(document.querySelector("[data-plan-status]")?.textContent).toBe("aprovado · em execução");
  });
});
