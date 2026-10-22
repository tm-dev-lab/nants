/*
 * Plano nos dois idiomas (spec 102, T02): em en, nenhum texto visível ou acessível do cartão do plano
 * fica em português (o conteúdo escrito pelo Arquiteto aparece como foi escrito); pt-BR igual a hoje;
 * a janela vazia, o subtítulo e o selo também seguem o idioma; a troca vale na hora.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState, SpecView } from "../../api/types";
import { notReady, renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState, makeSpec, makeTask, renderWith, resetStore } from "../../test-utils";
import { PlanCard, planReviewFixMessage, planWarnTitle } from "./PlanCard";
import { planSummary } from "./summary";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

/** Plano completo com conteúdo do Arquiteto em inglês (conteúdo não é traduzido). */
function fullSpec(extra: Partial<SpecView> = {}): SpecView {
  return {
    ...makeSpec("s1", "Online shop", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Cart", "TODO", { depends_on: ["T01"] })]),
    premises: ["Single store"],
    role_guidance: [{ role: "developer", text: "Keep it small" }],
    environment: { mode: "docker", services: ["web", "db"], base_image: "node:22", files: [{ path: ".nants/compose.yaml", content: "services: {}" }] },
    skills: [
      { role: "developer", stack: ["node"], reason: "New stack", status: "criado", updated_at: "2026-10-20T10:00:00Z", text: "**Node** expert" },
      { role: "reviewer", stack: [], status: "reaproveitado", updated_at: "2026-10-20T10:00:00Z", text: "Reviewer profile" },
    ],
    scope_warnings: ["T02 cites a path outside its scope"],
    plan_review: { status: "ok", issues: [{ task: "T01", kind: "scope", text: "Too broad" }] } as never,
    plan_changes: ["Added T02"],
    ...extra,
  };
}

function renderCard(lang: "pt-BR" | "en", spec: SpecView = fullSpec(), stage: "awaiting" | "planning" | "reviewing" | null = "awaiting") {
  resetStore({});
  return renderWith(<PlanCard projectId="p1" spec={spec} stage={stage} onApproved={async () => {}} onRequestFix={() => {}} />, { lang });
}

function card(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-plan]");
  if (el === null) throw new Error("plano ausente");
  return el;
}

function openAll(): void {
  for (const details of card().querySelectorAll("details")) details.open = true;
}

describe("Plano em en", () => {
  it("cartão completo sem português: tarefas, checagens, detalhe e rodapé", () => {
    renderCard("en");
    openAll();
    expect(portugueseLeftovers(card())).toEqual([]);
    expect(screen.getByText("2 tasks")).toBeTruthy();
    expect(screen.getByText("depends on T01")).toBeTruthy();
    expect(screen.getByText("The Reviewer made 1 finding and the automatic check found 1 warning")).toBeTruthy();
    expect(screen.getByText("Premises, guidance, environment, profiles, checks and what changed")).toBeTruthy();
    expect(screen.getByText(/Developer · created/)).toBeTruthy();
    expect(screen.getByText(/Reviewer · reused/)).toBeTruthy();
    expect(screen.getByText("Docker (approved together with the plan)")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Approve and start" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Ask for a fix" })).toBeTruthy();
    // Conteúdo do Arquiteto: como foi escrito.
    expect(screen.getByText(/Too broad/)).toBeTruthy();
    expect(screen.getByText("Keep it small")).toBeTruthy();
  });

  it("confirmação do início em en", () => {
    renderCard("en");
    fireEvent.click(screen.getByRole("button", { name: "Approve and start" }));
    expect(screen.getByText("Start the run now?")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Confirm start" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy();
    expect(portugueseLeftovers(card())).toEqual([]);
  });

  it("etapas planejando e revisando em en", () => {
    renderCard("en", fullSpec({ plan_review: { status: "running", issues: [] } as never }), "reviewing");
    expect(screen.getByText("Approve and Ask for a fix: waiting for the Reviewer…")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Approve and start" }).getAttribute("title")).toBe("waiting for the Reviewer");
    expect(portugueseLeftovers(card())).toEqual([]);
    cleanup();
    renderCard("en", fullSpec(), "planning");
    expect(screen.getAllByText("Plan in progress…").length).toBeGreaterThan(0);
    expect(portugueseLeftovers(card())).toEqual([]);
  });

  it("plano sem itens nas checagens e fora da aprovação em en", () => {
    renderCard("en", { ...makeSpec("s1", "", "IN_PROGRESS", [makeTask("T01", "Base", "TODO")]) }, null);
    expect(screen.getByText("Execution plan")).toBeTruthy();
    expect(screen.getByText("Plan checks: nothing to fix")).toBeTruthy();
    expect(screen.getByText("approved · running")).toBeTruthy();
    expect(portugueseLeftovers(card())).toEqual([]);
  });

  it("o pedido de correção sai no idioma ativo; o conteúdo dos itens fica como veio", () => {
    setLanguage("en", { persist: false });
    expect(planReviewFixMessage(["Scope warning"], [{ task: null, kind: "scope", text: "Too broad" }])).toBe(
      "Fix these points from the plan checks:\n- Scope warning\n- spec: Too broad",
    );
    expect(planWarnTitle(0, 2)).toBe("The automatic check found 2 warnings");
  });

  it("pt-BR igual a hoje e a troca vale na hora", () => {
    renderCard("pt-BR");
    expect(screen.getByText("O Revisor fez 1 apontamento e a verificação automática encontrou 1 aviso")).toBeTruthy();
    expect(screen.getByText("Premissas, orientações, ambiente, perfis, checagens e o que mudou")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Aprovar e iniciar" })).toBeTruthy();
    expect(portugueseLeftovers(card()).length).toBeGreaterThan(0);
    act(() => setLanguage("en", { persist: false }));
    expect(screen.getByRole("button", { name: "Approve and start" })).toBeTruthy();
    openAll();
    expect(portugueseLeftovers(card())).toEqual([]);
  });
});

describe("janela Plano em en (shell)", () => {
  const P1 = makeProject("p1", "Shop");
  const state = (spec: SpecView | null): ProjectState => makeProjectState({ id: "p1", name: "Shop", specs: spec === null ? [] : [spec] });

  it("estado vazio, prontidão e subtítulo em en", async () => {
    setLanguage("en", { persist: false });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(fullSpec()) }, readiness: notReady() });
    await settle();
    const plan = screen.getByRole("region", { name: "Plan" });
    const notice = within(plan).getByRole("region", { name: "To approve and start the run, you still need:" });
    expect(within(notice).getByRole("button", { name: "Set up: Docker daemon" })).toBeTruthy();
    expect(within(notice).getByText("Connection to a model provider")).toBeTruthy();
    const content = plan.querySelector<HTMLElement>("[data-plan]");
    if (content === null) throw new Error("plano ausente");
    for (const details of content.querySelectorAll("details")) details.open = true;
    expect(portugueseLeftovers(content)).toEqual([]);
  });

  it("subtítulo e selo do Plano", () => {
    setLanguage("en", { persist: false });
    expect(planSummary({ space: { activeSpec: null, planStage: null } } as never)).toEqual({
      subtitle: "no plan",
      empty: { message: "The Architect hasn't drafted a plan yet. Describe what you want in the Conversation." },
    });
    expect(planSummary({ space: { activeSpec: fullSpec(), planStage: "awaiting" } } as never)).toEqual({
      subtitle: "s1 · awaiting approval",
      badge: { count: 1, label: "Plan ready to approve" },
    });
    setLanguage("pt-BR", { persist: false });
    expect(planSummary({ space: { activeSpec: fullSpec(), planStage: "awaiting" } } as never)).toEqual({
      subtitle: "s1 · aguardando aprovação",
      badge: { count: 1, label: "Plano pronto para aprovar" },
    });
  });
});
