/* Janela Plano (spec 093, T06). */
/*
 * Spec 080/T02: com turno do Arquiteto ativo, a janela Plano mostra "Plano em elaboração…" no
 * lugar da contagem e das ações de aprovar; 409 `architect_turn_active`/`plan_without_tasks` têm
 * mensagem clara.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { PlanCard } from "./PlanCard";
import { approveProblem } from "../shared/runProblems";
import { approveSpec } from "../../api/client";
import { appStore } from "../../store";
import { APPROVE_REASON_MESSAGES, PLAN_DRAFTING_NOTICE } from "../../copy";
import { installFetch, makeSpec, makeTask, resetStore } from "../../test-utils";

const APPROVE = "Aprovar e iniciar";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("PlanCard: plano em elaboração", () => {
  it("turno ativo esconde contagem e aprovar; fim do turno devolve as ações", () => {
    resetStore({});
    appStore.syncArchitectTurn("p1", { turn_id: "t1", user_text: "plano", started_at: 1 });
    const spec = makeSpec("s1", "API", "AWAITING_APPROVAL", []);
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);

    expect(screen.getAllByText(PLAN_DRAFTING_NOTICE).length).toBeGreaterThan(0);
    expect(screen.queryByText("0 tarefas")).toBeNull();
    expect(screen.queryByRole("button", { name: APPROVE })).toBeNull();

    act(() => appStore.syncArchitectTurn("p1", null));

    expect(screen.queryByText(PLAN_DRAFTING_NOTICE)).toBeNull();
    expect(screen.getByText("0 tarefas")).not.toBeNull();
    expect(screen.getByRole("button", { name: APPROVE })).not.toBeNull();
  });

  it("turno de outro projeto não afeta o cartão", () => {
    resetStore({});
    appStore.syncArchitectTurn("p2", { turn_id: "t9", user_text: "x", started_at: 1 });
    const spec = makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]);
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(screen.getByText("1 tarefa")).not.toBeNull();
    expect(screen.getByRole("button", { name: APPROVE })).not.toBeNull();
  });
});

describe("aprovação recusada com turno ativo ou plano sem tarefas", () => {
  it.each(["architect_turn_active", "plan_without_tasks"])("409 %s chega com o reason e vira mensagem clara", async (reason) => {
    resetStore({});
    installFetch(() => ({ status: 409, body: { error: "recusado", reason } }));
    const result = await approveSpec("p1", "s1");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.reason).toBe(reason);
    const expected = APPROVE_REASON_MESSAGES[reason];
    expect(expected).toBeDefined();
    expect(approveProblem(result.error).message).toBe(expected);
  });

  it("o cartão mostra a mensagem do 409 ao aprovar", async () => {
    resetStore({});
    installFetch(() => ({ status: 409, body: { error: "recusado", reason: "plan_without_tasks" } }));
    const spec = makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]);
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: APPROVE }));
    await user.click(screen.getByRole("button", { name: "Confirmar início" }));
    expect((await screen.findByRole("alert")).textContent).toContain(APPROVE_REASON_MESSAGES.plan_without_tasks ?? "");
  });
});
