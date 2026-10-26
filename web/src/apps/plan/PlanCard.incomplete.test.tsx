/*
 * Plano incompleto no PlanCard (spec 127 T01): a spec em aprovação sem nenhuma tarefa (o turno do
 * Arquiteto caiu antes de gravá-las) não fica "revisando"; o cartão diz que o plano está
 * incompleto, mantém a aprovação bloqueada e oferece "Pedir ao Arquiteto para terminar", que manda
 * a continuação interna pelo servidor.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { PlanCard } from "./PlanCard";
import { approveSpec, postChat, postChatContinue } from "../../api/client";
import { setLanguage } from "../../i18n";
import { en } from "../../i18n/en";
import { ptBR } from "../../i18n/ptBR";
import type { SpecView } from "../../api/types";
import { appStore } from "../../store";
import { makeSpec, makeTask, resetStore } from "../../test-utils";

vi.mock("../../api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api/client")>()),
  approveSpec: vi.fn(),
  postChat: vi.fn(),
  postChatContinue: vi.fn(),
}));
vi.mock("../../chatHistory", () => ({ loadChatHistory: vi.fn(async () => {}) }));

const continueMock = vi.mocked(postChatContinue);

afterEach(() => {
  cleanup();
  continueMock.mockReset();
  vi.mocked(approveSpec).mockReset();
  vi.mocked(postChat).mockReset();
  setLanguage("pt-BR");
});

function incompleteSpec(): SpecView {
  return { ...makeSpec("001-plano", "Planner", "AWAITING_APPROVAL", []), plan_review: { status: "incomplete", issues: [] } };
}

function finishButton(): HTMLButtonElement | null {
  return document.querySelector<HTMLButtonElement>("[data-plan-finish]");
}

describe("PlanCard: plano sem tarefas", () => {
  it("diz que o plano está incompleto, sem 'revisando', e mantém a aprovação bloqueada", () => {
    resetStore({});
    render(<PlanCard projectId="p1" spec={incompleteSpec()} onApproved={async () => {}} />);
    expect(screen.getByText(ptBR["plan.incomplete.text"])).toBeTruthy();
    expect(finishButton()?.textContent).toContain(ptBR["plan.incomplete.finish"]);
    const approve = document.querySelector<HTMLButtonElement>("[data-approve]");
    expect(approve?.disabled).toBe(true);
    expect(screen.queryByText(ptBR["plan.review.running"])).toBeNull();
    expect(screen.queryByText(ptBR["plan.review.waiting"])).toBeNull();
    expect(screen.queryByText(ptBR["plan.review.stale"])).toBeNull();
    expect(screen.getByText(ptBR["plan.review.incomplete"])).toBeTruthy();
    act(() => setLanguage("en"));
    expect(screen.getByText(en["plan.incomplete.text"])).toBeTruthy();
    expect(finishButton()?.textContent).toContain(en["plan.incomplete.finish"]);
  });

  it("Pedir ao Arquiteto para terminar manda a continuação interna da spec", async () => {
    resetStore({});
    const show = vi.spyOn(appStore, "showArchitectReply");
    const onApproved = vi.fn(async () => {});
    continueMock.mockResolvedValue({ ok: true, data: { reply: "Tarefas gravadas.", draft_updated: true, turn_id: "0123456789abcdef0123456789abcdef" } });
    render(<PlanCard projectId="p1" spec={incompleteSpec()} onApproved={onApproved} />);
    const button = finishButton();
    if (button === null) throw new Error("sem botão");
    await act(async () => {
      fireEvent.click(button);
    });
    expect(continueMock).toHaveBeenCalledWith("p1", "001-plano");
    expect(vi.mocked(postChat)).not.toHaveBeenCalled();
    expect(vi.mocked(approveSpec)).not.toHaveBeenCalled();
    expect(show).toHaveBeenCalledWith("p1", "Tarefas gravadas.", "0123456789abcdef0123456789abcdef");
    expect(onApproved).toHaveBeenCalled();
    show.mockRestore();
  });

  it("com tarefas, o cartão volta ao fluxo de sempre", () => {
    resetStore({});
    const spec = { ...makeSpec("001-plano", "Planner", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]), plan_review: { status: "ok", issues: [] } };
    render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} />);
    expect(screen.queryByText(ptBR["plan.incomplete.text"])).toBeNull();
    expect(finishButton()).toBeNull();
    expect(document.querySelector<HTMLButtonElement>("[data-approve]")?.disabled).toBe(false);
  });
});
