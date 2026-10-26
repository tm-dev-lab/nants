/*
 * Consenso do plano no PlanCard (spec 128 T05): o progresso durante o ciclo, com a aprovação
 * bloqueada; o resumo no fim (apontamentos, ajustados, mantidos com o motivo recolhidos, tamanho do
 * plano); os pontos em aberto, reabertos e em disputa com as duas posições; o plano pronto sem
 * aviso; o botão que manda só os pontos em aberto; e a confirmação que avisa a renumeração.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { PlanCard, architectPosition, consensusPending, planReviewFixMessage } from "./PlanCard";
import { setLanguage } from "../../i18n";
import type { ConsensusView, PlanReviewView, SpecView } from "../../api/types";
import { makeSpec, makeTask, resetStore } from "../../test-utils";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
});

const BASE: ConsensusView = {
  round: 0,
  max_rounds: 3,
  evaluating: 0,
  adjusted: 0,
  kept: 0,
  unanswered: 0,
  open: [],
  disputed: [],
  kept_points: [],
  reopened: [],
  total: 0,
  tasks_first: 0,
  criteria_first: 0,
  tasks: 0,
  criteria: 0,
};

const KEPT = { id: "P3", task: "T04", kind: "scope", text: "Precisa de arquivos em docs/.", decision: "reject", reason_kind: "not_worth_cost", reason: "A tarefa não escreve em docs/." };
const OPEN = { id: "P5", task: "T02", kind: "untestable", text: "O critério de cache não é verificável." };
const REOPENED = {
  id: "P1",
  task: "T03",
  kind: "scope",
  text: "O login novo exige o teste de sessão.",
  decision: "reject",
  reason_kind: "inflates_criteria",
  reason: "O teste de sessão já está na T02.",
};
const DISPUTED = {
  id: "P2",
  task: "T05",
  kind: "untestable",
  text: "O teste confere strings literais do README.",
  decision: "reject",
  reason_kind: "other",
  reason: "O texto do README é o entregável.",
};

function renderWith(review: PlanReviewView, onRequestFix?: (text: string) => void, extra: Partial<SpecView> = {}) {
  resetStore({});
  const spec: SpecView = {
    ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Login", "TODO"), makeTask("T03", "Sessão", "TODO")]),
    plan_review: review,
    ...extra,
  };
  render(<PlanCard projectId="p1" spec={spec} onApproved={async () => {}} {...(onRequestFix !== undefined ? { onRequestFix } : {})} />);
}

function approveButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: /Aprovar e iniciar/ }) as HTMLButtonElement;
}

describe("PlanCard: consenso em curso", () => {
  it("Revisor conferindo na revisão inicial, sem rodada, com a aprovação bloqueada", () => {
    renderWith({ status: "running", issues: [], consensus: { ...BASE, phase: "review" } });
    expect(screen.getByText("O Revisor está conferindo o plano…")).toBeTruthy();
    expect(document.querySelector("[data-plan-round]")).toBeNull();
    expect(approveButton().disabled).toBe(true);
  });

  it("Arquiteto avaliando N apontamentos na rodada 2 de 3, com a aprovação bloqueada", () => {
    const onRequestFix = vi.fn();
    renderWith({ status: "running", issues: [], consensus: { ...BASE, round: 2, phase: "architect", evaluating: 3 } }, onRequestFix, {
      scope_warnings: ["T01: fora do escopo"],
    });
    expect(screen.getByText("O Arquiteto está avaliando 3 apontamentos…")).toBeTruthy();
    expect(screen.getByText("Rodada 2 de 3")).toBeTruthy();
    expect(approveButton().disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Pedir correção" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("um apontamento usa o singular", () => {
    renderWith({ status: "running", issues: [], consensus: { ...BASE, round: 1, phase: "architect", evaluating: 1 } });
    expect(screen.getByText("O Arquiteto está avaliando 1 apontamento…")).toBeTruthy();
    expect(screen.getByText("Rodada 1 de 3")).toBeTruthy();
  });
});

describe("PlanCard: consenso concluído", () => {
  const AGREED: ConsensusView = {
    ...BASE,
    round: 2,
    outcome: "consensus",
    adjusted: 3,
    kept: 1,
    total: 4,
    kept_points: [KEPT],
    tasks_first: 10,
    criteria_first: 26,
    tasks: 12,
    criteria: 30,
  };

  it("sem ponto em aberto: plano pronto, sem aviso, com o resumo e os mantidos recolhidos", () => {
    renderWith({ status: "ok", issues: [{ id: "P3", task: "T04", kind: "scope", text: KEPT.text }], consensus: AGREED }, vi.fn());
    const ready = document.querySelector("[data-plan-ready]") as HTMLElement;
    expect(within(ready).getByText("Plano pronto: o Revisor e o Arquiteto chegaram a um acordo.")).toBeTruthy();
    expect(within(ready).getByText("Revisão do plano, 4 apontamentos: 3 ajustados e 1 mantido.")).toBeTruthy();
    expect(within(ready).getByText("Tamanho do plano: 12 tarefas (eram 10 na primeira versão); 30 critérios de aceitação (eram 26).")).toBeTruthy();
    // Os mantidos ficam recolhidos, com o motivo pelo reason_kind e a frase do Arquiteto.
    const kept = ready.querySelector("[data-plan-kept]") as HTMLDetailsElement;
    expect(kept.open).toBe(false);
    expect(within(kept).getByText("1 apontamento mantido pelo Arquiteto, com o motivo")).toBeTruthy();
    const item = kept.querySelector('[data-plan-kept-point="P3"]') as HTMLElement;
    expect(item.textContent).toContain("Precisa de arquivos em docs/.");
    expect(item.textContent).toContain("Arquiteto: não compensa o custo: A tarefa não escreve em docs/.");
    // Nada de aviso de problema nem botão de correção; a aprovação é livre.
    expect(document.querySelector("[data-plan-warn]")).toBeNull();
    expect(screen.queryByText("Checagens do plano: nada a corrigir")).toBeNull();
    expect(screen.queryByRole("button", { name: "Pedir correção" })).toBeNull();
    expect(approveButton().disabled).toBe(false);
  });

  it("plano sem nenhum apontamento: pronto, sem resumo de números", () => {
    renderWith({ status: "ok", issues: [], consensus: { ...BASE, outcome: "consensus", tasks_first: 3, criteria_first: 6, tasks: 3, criteria: 6 } });
    expect(screen.getByText("Plano pronto: o Revisor não encontrou nada a corrigir.")).toBeTruthy();
    expect(document.querySelector("[data-plan-consensus-summary]")).toBeNull();
    expect(document.querySelector("[data-plan-warn]")).toBeNull();
  });

  it("sem o total do servidor, a mesma soma por ponto", () => {
    const { total: _omit, ...withoutTotal } = { ...AGREED, open: [OPEN] };
    renderWith({ status: "ok", issues: [], consensus: withoutTotal }, vi.fn());
    expect(screen.getByText("Revisão do plano, 5 apontamentos: 3 ajustados, 1 mantido e 1 em aberto.")).toBeTruthy();
  });

  it("tamanho igual ao da primeira versão", () => {
    renderWith({ status: "ok", issues: [], consensus: { ...AGREED, kept_points: [], tasks_first: 1, tasks: 1, criteria_first: 4, criteria: 4 } });
    expect(screen.getByText("Tamanho do plano: 1 tarefa, como na primeira versão; 4 critérios de aceitação, como na primeira versão.")).toBeTruthy();
  });
});

describe("PlanCard: pontos em aberto e em disputa", () => {
  const OPEN_AT_CAP: ConsensusView = {
    ...BASE,
    round: 3,
    outcome: "open",
    adjusted: 2,
    kept: 1,
    total: 6,
    open: [OPEN],
    reopened: [REOPENED],
    disputed: [DISPUTED],
    kept_points: [KEPT],
    tasks_first: 10,
    criteria_first: 26,
    tasks: 11,
    criteria: 27,
  };
  const ISSUES = [OPEN, REOPENED, DISPUTED, KEPT].map(({ id, task, kind, text }) => ({ id, task, kind, text }));

  it("destaca os pontos com o apontamento do Revisor e a posição do Arquiteto", () => {
    renderWith({ status: "ok", issues: ISSUES, consensus: OPEN_AT_CAP }, vi.fn());
    const band = screen.getByRole("region", { name: "Checagens do plano" });
    expect(within(band).getByRole("heading", { name: "3 pontos em aberto entre o Revisor e o Arquiteto" })).toBeTruthy();
    const points = band.querySelectorAll("[data-plan-point]");
    expect([...points].map((p) => p.getAttribute("data-plan-point"))).toEqual(["open", "reopened", "disputed"]);
    const [open, reopened, disputed] = [...points] as HTMLElement[];
    expect(open?.textContent).toContain("Revisor: O critério de cache não é verificável.");
    expect(open?.textContent).toContain("Arquiteto: ainda não respondeu a este ponto.");
    expect(within(reopened as HTMLElement).getByText("reaberto pelo Revisor")).toBeTruthy();
    expect(reopened?.textContent).toContain("Revisor: O login novo exige o teste de sessão.");
    expect(reopened?.textContent).toContain("Arquiteto: inflaria os critérios: O teste de sessão já está na T02.");
    expect(within(disputed as HTMLElement).getByText("em disputa")).toBeTruthy();
    expect(disputed?.textContent).toContain("Arquiteto: outro motivo: O texto do README é o entregável.");
    // O mantido fora da disputa não entra na faixa; fica no resumo, recolhido.
    expect(within(band).queryByText(/Precisa de arquivos em docs/)).toBeNull();
    expect(screen.getByText("Revisão do plano, 6 apontamentos: 2 ajustados, 1 mantido e 3 em aberto.")).toBeTruthy();
    // Total = ajustados + mantidos + em aberto, reabertos e em disputa.
    const c = OPEN_AT_CAP;
    expect(c.total).toBe(c.adjusted + c.kept + c.open.length + (c.reopened ?? []).length + c.disputed.length);
    expect(within(band).getByText("Sem acordo em 3 rodadas. A decisão é sua: peça o ajuste ao Arquiteto ou aprove assim mesmo.")).toBeTruthy();
    // Aprovar mesmo assim continua possível.
    expect(approveButton().disabled).toBe(false);
  });

  it("o botão manda ao Arquiteto só os pontos em aberto ou em disputa, com as duas posições", () => {
    const onRequestFix = vi.fn();
    renderWith({ status: "ok", issues: ISSUES, consensus: OPEN_AT_CAP }, onRequestFix);
    fireEvent.click(screen.getByRole("button", { name: "Pedir correção" }));
    expect(onRequestFix).toHaveBeenCalledWith(
      "Corrija estes pontos das checagens do plano:\n" +
        "- T02 (P5): O critério de cache não é verificável.\n" +
        "- T03 (P1): O login novo exige o teste de sessão.\n" +
        "  Posição do Arquiteto: inflaria os critérios: O teste de sessão já está na T02.\n" +
        "- T05 (P2): O teste confere strings literais do README.\n" +
        "  Posição do Arquiteto: outro motivo: O texto do README é o entregável.",
    );
    const message = onRequestFix.mock.calls[0]?.[0] as string;
    expect(message).not.toContain("docs/");
  });

  it("só disputa: o desfecho explica e o botão leva a disputa e os avisos automáticos", () => {
    const onRequestFix = vi.fn();
    const consensus: ConsensusView = { ...BASE, round: 1, outcome: "disputed", kept: 0, total: 1, disputed: [DISPUTED] };
    renderWith({ status: "ok", issues: [{ ...ISSUES[2]!, disputed: true }], consensus }, onRequestFix, { scope_warnings: ["T01: fora do escopo"] });
    const band = screen.getByRole("region", { name: "Checagens do plano" });
    expect(within(band).getByRole("heading", { name: "1 ponto em aberto entre o Revisor e o Arquiteto e a verificação automática encontrou 1 aviso" })).toBeTruthy();
    expect(within(band).getByText(/^O Revisor e o Arquiteto mantêm posições diferentes\. A decisão é sua/)).toBeTruthy();
    // F-01: a disputa conta só como em aberto; o resumo fecha a conta.
    expect(screen.getByText("Revisão do plano, 1 apontamento: 0 ajustados, 0 mantidos e 1 em aberto.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Pedir correção" }));
    expect(onRequestFix).toHaveBeenCalledWith(
      "Corrija estes pontos das checagens do plano:\n" +
        "- T01: fora do escopo\n" +
        "- T05 (P2): O teste confere strings literais do README.\n" +
        "  Posição do Arquiteto: outro motivo: O texto do README é o entregável.",
    );
  });

  it("reaberto no teto com a lista open vazia continua em aberto (critério 11)", () => {
    const consensus: ConsensusView = { ...BASE, round: 3, outcome: "open", kept: 0, total: 1, reopened: [REOPENED] };
    renderWith({ status: "ok", issues: [ISSUES[1]!], consensus }, vi.fn());
    expect(document.querySelector("[data-plan-ready]")).toBeNull();
    expect(document.querySelector('[data-plan-point="reopened"]')).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pedir correção" })).toBeTruthy();
  });

  it("os pontos à espera e a posição do Arquiteto, como funções", () => {
    expect(consensusPending(OPEN_AT_CAP).map((p) => [p.id, p.state])).toEqual([
      ["P5", "open"],
      ["P1", "reopened"],
      ["P2", "disputed"],
    ]);
    expect(architectPosition({ reason_kind: "duplicate" })).toBe("repete outro ponto");
    expect(architectPosition({ reason: "Só a frase." })).toBe("Só a frase.");
    expect(architectPosition({ reason_kind: "desconhecido", reason: "" })).toBeNull();
    // Registro antigo (sem consenso): a mensagem de antes, sem id nem posição.
    expect(planReviewFixMessage([], [{ id: "P9", task: "T01", kind: "scope", text: "Texto." }])).toBe("Corrija estes pontos das checagens do plano:\n- T01: Texto.");
  });

  it("em inglês, os textos seguem o idioma", () => {
    setLanguage("en", { persist: false });
    const onRequestFix = vi.fn();
    renderWith({ status: "ok", issues: ISSUES, consensus: OPEN_AT_CAP }, onRequestFix);
    expect(screen.getByRole("heading", { name: "3 points still open between the Reviewer and the Architect" })).toBeTruthy();
    expect(screen.getByText("Plan review, 6 findings: 2 adjusted, 1 kept and 3 open.")).toBeTruthy();
    expect(screen.getByText("Plan size: 11 tasks (there were 10 in the first version); 27 acceptance criteria (there were 26).")).toBeTruthy();
    expect(document.querySelector('[data-plan-point="disputed"]')?.textContent).toContain("Architect: other reason: O texto do README é o entregável.");
    fireEvent.click(screen.getByRole("button", { name: "Ask for a fix" }));
    expect(onRequestFix.mock.calls[0]?.[0]).toContain("  Architect's position: would inflate the criteria: O teste de sessão já está na T02.");
  });
});

describe("PlanCard: aprovação avisa a renumeração (C-07)", () => {
  it("com a ordem de execução diferente dos ids, a confirmação mostra o mapa", () => {
    renderWith({ status: "ok", issues: [], consensus: { ...BASE, outcome: "consensus" } }, undefined, { execution_order: ["T01", "T03", "T02"] });
    const ids = [...document.querySelectorAll(".nd-plan-task-id")].map((n) => n.textContent);
    expect(ids).toEqual(["T01", "T03", "T02"]);
    fireEvent.click(approveButton());
    const note = document.querySelector("[data-plan-renumber]") as HTMLElement;
    expect(note.textContent).toBe("Na aprovação, as tarefas serão renumeradas na ordem de execução da lista: T03 passa a T02, T02 passa a T03.");
    expect(screen.getByRole("group", { name: "Iniciar a execução agora?" }).getAttribute("aria-describedby")).toBe(note.id);
  });

  it("com a ordem igual aos ids, sem aviso", () => {
    renderWith({ status: "ok", issues: [], consensus: { ...BASE, outcome: "consensus" } }, undefined, { execution_order: ["T01", "T02", "T03"] });
    fireEvent.click(approveButton());
    expect(screen.getByText("Iniciar a execução agora?")).toBeTruthy();
    expect(document.querySelector("[data-plan-renumber]")).toBeNull();
  });
});
