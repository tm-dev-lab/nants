/* Janela Plano (spec 093, T06). */
/*
 * Checagens do plano no PlanCard (spec 092/T02; revisão do plano da spec 086/T08; desenho da
 * 095/T04): faixa âmbar única na vista principal com as duas origens ("Verificação automática do
 * Nants" e "Revisor") e o título "O Revisor fez N apontamentos"; a situação por origem no
 * `<details>` (aberto pelo `summary` antes da busca, C-11); um único "Pedir correção" no rodapé com
 * o texto combinado; a linha "nada a corrigir"; e o botão de aprovar, que espera a revisão da
 * versão atual (pending/running) e libera em stale (com aviso), ok e indisponível.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { PLAN_ACTIONS_WAITING_REASON, PLAN_REVIEW_WAITING } from "../../copy";
import { PLAN_REVIEW_RECHECK_MS } from "../../desktop/useSpaceData";
import { PlanCard, planMoreLabel, planReviewFixMessage, planWarnTitle, tooLargeIssues } from "./PlanCard";
import { setLanguage } from "../../i18n";
import type { PlanReviewView, SpecView } from "../../api/types";
import { makeSpec, makeTask, resetStore } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

interface Extra {
  review?: PlanReviewView;
  scope?: string[];
  coverage?: string[];
}

function renderWith(extra: Extra, onRequestFix?: (text: string) => void, onApproved = async () => {}) {
  resetStore({});
  const spec: SpecView = {
    ...makeSpec("s1", "API", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]),
    ...(extra.review !== undefined ? { plan_review: extra.review } : {}),
    ...(extra.scope !== undefined ? { scope_warnings: extra.scope } : {}),
    ...(extra.coverage !== undefined ? { coverage_warnings: extra.coverage } : {}),
  };
  render(
    <PlanCard
      projectId="p1"
      spec={spec}
      onApproved={onApproved}
      {...(onRequestFix !== undefined ? { onRequestFix } : {})}
    />,
  );
}

/** Abre o `<details>` do plano pelo `summary` (C-11) e devolve o corpo. */
function openMore(): HTMLElement {
  const details = document.querySelector<HTMLDetailsElement>("[data-plan-more]");
  if (details === null) throw new Error("detalhes ausentes");
  expect(details.open).toBe(false);
  const summary = details.querySelector("summary");
  if (summary === null) throw new Error("summary ausente");
  fireEvent.click(summary);
  details.open = true;
  return details;
}

function approveButton(): HTMLButtonElement {
  return screen.getByRole("button", { name: /Aprovar e iniciar/ }) as HTMLButtonElement;
}

const WAITING = PLAN_REVIEW_WAITING;
const STALE_NOTICE = "A revisão está demorando; você pode aprovar assim mesmo.";
const CLEAN = "Checagens do plano: nada a corrigir";
const SCOPE = "T01: critério cita `deps.edn`, fora do escopo";
const COVERAGE = "G2 (Logout limpa a sessão.) não é coberto por nenhuma tarefa";
const ISSUES = [
  { task: "T02", kind: "contradiction", text: "Pontuação de 0 a 5, mas as regras só permitem 5." },
  { task: null, kind: "uncovered_criterion", text: "G3 não é coberto por nenhuma tarefa." },
];

describe("PlanCard: checagens do plano", () => {
  it("faixa âmbar única com as duas origens, título do Revisor e sem caixa", () => {
    renderWith({ review: { status: "ok", issues: ISSUES }, scope: [SCOPE], coverage: [COVERAGE] });
    const block = screen.getByRole("region", { name: "Checagens do plano" });
    expect(block.hasAttribute("data-plan-warn")).toBe(true);
    expect(within(block).getByRole("heading", { name: "O Revisor fez 2 apontamentos e a verificação automática encontrou 2 avisos" })).toBeTruthy();
    const auto = within(block).getByRole("region", { name: "Verificação automática do Nants" });
    const reviewer = within(block).getByRole("region", { name: "Revisor" });
    expect(within(auto).getByText(SCOPE)).toBeTruthy();
    expect(within(auto).getByText(COVERAGE)).toBeTruthy();
    expect(within(reviewer).getByText("T02")).toBeTruthy();
    expect(within(reviewer).getByText(/Pontuação de 0 a 5/)).toBeTruthy();
    expect(within(reviewer).getByText("spec")).toBeTruthy();
    expect(within(block).getByText("Apenas um aviso: você ainda pode aprovar.")).toBeTruthy();
    // Nenhum aviso solto fora da faixa.
    expect(screen.queryByRole("heading", { name: "Avisos de escopo" })).toBeNull();
    expect(screen.queryByRole("heading", { name: "Revisão do plano" })).toBeNull();
    // Fora da lista de tarefas e antes do botão de aprovar; os itens nunca bloqueiam.
    expect(screen.getByText("Base").closest("section")).not.toBe(block);
    expect(block.compareDocumentPosition(approveButton()) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(approveButton().disabled).toBe(false);
  });

  it("o detalhe recolhido traz a explicação e a situação de cada origem, sem repetir os itens", () => {
    renderWith({ review: { status: "ok", issues: ISSUES }, scope: [SCOPE] });
    const more = openMore();
    const detail = within(more).getByRole("region", { name: "Sobre as checagens" });
    expect(within(detail).getByText("Segunda opinião sobre o plano; o Arquiteto pode corrigir")).toBeTruthy();
    expect(within(detail).getByText("1 aviso na faixa acima")).toBeTruthy();
    expect(within(detail).getByText("2 apontamentos na faixa acima")).toBeTruthy();
    expect(within(detail).queryByText(SCOPE)).toBeNull();
  });

  it("título da faixa por origem e rótulo do detalhe", () => {
    expect(planWarnTitle(1, 0)).toBe("O Revisor fez 1 apontamento");
    expect(planWarnTitle(0, 2)).toBe("A verificação automática encontrou 2 avisos");
    expect(planWarnTitle(2, 1)).toBe("O Revisor fez 2 apontamentos e a verificação automática encontrou 1 aviso");
    expect(planMoreLabel(["premissas", "ambiente", "perfis"])).toBe("Premissas, ambiente e perfis");
    expect(planMoreLabel(["checagens"])).toBe("Checagens");
    expect(planMoreLabel([])).toBe("");
  });

  it("um único botão envia todos os itens das duas origens", () => {
    const onRequestFix = vi.fn();
    renderWith({ review: { status: "ok", issues: ISSUES }, scope: [SCOPE], coverage: [COVERAGE] }, onRequestFix);
    const buttons = screen.getAllByRole("button", { name: "Pedir correção" });
    expect(buttons).toHaveLength(1);
    fireEvent.click(buttons[0] as HTMLElement);
    expect(onRequestFix).toHaveBeenCalledTimes(1);
    expect(onRequestFix).toHaveBeenCalledWith(
      "Corrija estes pontos das checagens do plano:\n" +
        `- ${SCOPE}\n` +
        `- ${COVERAGE}\n` +
        "- T02: Pontuação de 0 a 5, mas as regras só permitem 5.\n" +
        "- spec: G3 não é coberto por nenhuma tarefa.",
    );
  });

  it("só avisos automáticos com a revisão em curso: o Revisor mostra revisando e o botão espera o Revisor (094)", () => {
    const onRequestFix = vi.fn();
    renderWith({ review: { status: "running", issues: [] }, scope: [SCOPE] }, onRequestFix);
    const reviewer = screen.getByRole("region", { name: "Revisor" });
    expect(within(reviewer).getByText("O Revisor está revisando o plano…")).toBeTruthy();
    const fix = screen.getByRole("button", { name: "Pedir correção" }) as HTMLButtonElement;
    expect(fix.disabled).toBe(true);
    expect(fix.title).toBe(PLAN_ACTIONS_WAITING_REASON);
    fireEvent.click(fix);
    expect(onRequestFix).not.toHaveBeenCalled();
  });

  it("revisão concluída: o botão envia os avisos", () => {
    const onRequestFix = vi.fn();
    renderWith({ review: { status: "ok", issues: [] }, scope: [SCOPE] }, onRequestFix);
    fireEvent.click(screen.getByRole("button", { name: "Pedir correção" }));
    expect(onRequestFix).toHaveBeenCalledWith(`Corrija estes pontos das checagens do plano:\n- ${SCOPE}`);
  });

  it("só apontamentos do Revisor: a faixa é do Revisor e o detalhe diz que a verificação automática não tem avisos", () => {
    renderWith({ review: { status: "ok", issues: ISSUES } });
    expect(screen.getByRole("heading", { name: "O Revisor fez 2 apontamentos" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Verificação automática do Nants" })).toBeNull();
    const more = openMore();
    expect(within(more).getByText("Sem avisos.")).toBeTruthy();
  });

  it("sem itens e revisão concluída: só a linha nada a corrigir", () => {
    renderWith({ review: { status: "ok", at: "2026-10-18T00:00:00Z", issues: [] }, scope: [], coverage: [] });
    expect(screen.getByText(CLEAN)).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Checagens do plano" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Pedir correção" })).toBeNull();
    expect(approveButton().disabled).toBe(false);
  });

  it("sem Revisor e sem avisos: nada a corrigir e Aprovar habilitado", () => {
    renderWith({});
    expect(screen.getByText(CLEAN)).toBeTruthy();
    expect(screen.queryByText(WAITING)).toBeNull();
    expect(approveButton().disabled).toBe(false);
  });

  it("sem Revisor e com avisos: só a subseção automática", () => {
    renderWith({ coverage: [COVERAGE] });
    expect(screen.getByRole("region", { name: "Verificação automática do Nants" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Revisor" })).toBeNull();
    expect(screen.getByRole("heading", { name: "A verificação automática encontrou 1 aviso" })).toBeTruthy();
    expect(screen.queryByText(CLEAN)).toBeNull();
    expect(approveButton().disabled).toBe(false);
  });

  it("revisão indisponível vira uma linha discreta e não bloqueia", () => {
    renderWith({ review: { status: "unavailable", issues: [] } });
    expect(screen.getByText("Revisão indisponível.")).toBeTruthy();
    expect(screen.queryByText(CLEAN)).toBeNull();
    expect(approveButton().disabled).toBe(false);
  });

  it("spec 133 T11: revisão indisponível pelo provedor diz o papel, a conexão, o modelo e o motivo, nos dois idiomas", () => {
    const failure = { provider: "provider_http_404", role: "reviewer", model: "gemini-x", connection_id: "gem", connection_name: "Gemini", category: "bad_request", status: 404, error_type: "not_found" };
    renderWith({ review: { status: "unavailable", issues: [], failure } });
    const line = document.querySelector("[data-plan-review-failure]");
    expect(line?.textContent).toContain("Revisão indisponível.");
    expect(line?.textContent).toContain("Revisor");
    expect(line?.textContent).toContain("gemini-x");
    expect(line?.querySelector("[data-provider-evidence]")?.textContent).toContain("not_found");
    expect(approveButton().disabled).toBe(false);
    act(() => setLanguage("en", { persist: false }));
    expect(document.querySelector("[data-plan-review-failure]")?.textContent).toContain("Review unavailable.");
    expect(document.querySelector("[data-plan-review-failure]")?.textContent).toContain("Reviewer");
    act(() => setLanguage("pt-BR", { persist: false }));
  });

  it("monta a mensagem a partir dos avisos e das issues", () => {
    expect(planReviewFixMessage([SCOPE], ISSUES)).toBe(
      `Corrija estes pontos das checagens do plano:\n- ${SCOPE}\n- T02: Pontuação de 0 a 5, mas as regras só permitem 5.\n- spec: G3 não é coberto por nenhuma tarefa.`,
    );
  });
});

describe("PlanCard: aprovação espera a revisão da versão atual", () => {
  it.each(["pending", "running"])("%s: Aprovar desabilitado com 'aguardando o Revisor'", (status) => {
    renderWith({ review: { status, issues: [] } });
    expect(approveButton().disabled).toBe(true);
    expect(approveButton().title).toBe(PLAN_ACTIONS_WAITING_REASON);
    expect(WAITING).toContain(PLAN_ACTIONS_WAITING_REASON);
    expect(screen.getByText(WAITING)).toBeTruthy();
    expect(screen.queryByText(STALE_NOTICE)).toBeNull();
    // Sem itens, a revisão em curso aparece na vista principal (fora do detalhe).
    const running = screen.getByText("O Revisor está revisando o plano…");
    expect(running.closest("details")).toBeNull();
  });

  it("pending com avisos: Aprovar e Pedir correção esperam o Revisor (094, decisão 3)", () => {
    const onRequestFix = vi.fn();
    renderWith({ review: { status: "pending", issues: [] }, scope: [SCOPE] }, onRequestFix);
    const fix = screen.getByRole("button", { name: "Pedir correção" }) as HTMLButtonElement;
    expect(fix.disabled).toBe(true);
    expect(approveButton().disabled).toBe(true);
    expect(fix.getAttribute("aria-describedby")).toBe(screen.getByText(WAITING).id);
  });

  it("stale: Aprovar habilitado com o aviso de demora", () => {
    renderWith({ review: { status: "stale", issues: [] } });
    expect(approveButton().disabled).toBe(false);
    expect(screen.getByText(STALE_NOTICE)).toBeTruthy();
    expect(screen.queryByText(WAITING)).toBeNull();
    expect(screen.getByText("O Revisor ainda não respondeu.")).toBeTruthy();
  });

  it("ok com apontamentos: os itens nunca bloqueiam a aprovação", () => {
    renderWith({ review: { status: "ok", issues: ISSUES }, scope: [SCOPE] });
    expect(approveButton().disabled).toBe(false);
    expect(screen.queryByText(WAITING)).toBeNull();
    expect(screen.queryByText(STALE_NOTICE)).toBeNull();
  });

  it("o cartão não reconsulta o estado: o poll da revisão é do espaço (useSpaceData, 094)", () => {
    vi.useFakeTimers();
    const reload = vi.fn(async () => {});
    renderWith({ review: { status: "pending", issues: [] } }, undefined, reload);
    act(() => {
      vi.advanceTimersByTime(PLAN_REVIEW_RECHECK_MS * 2);
    });
    expect(reload).not.toHaveBeenCalled();
  });
});

describe("PlanCard: tarefa grande demais (spec 126 T03)", () => {
  const BIG = { task: "T01", kind: "too_large", text: "Mistura servidor e cliente; divida em duas tarefas." };

  afterEach(() => {
    setLanguage("pt-BR", { persist: false });
  });

  it("o too_large que restou vem primeiro, com a tarefa em destaque e a sugestão; a aprovação continua", () => {
    renderWith({ review: { status: "ok", issues: [ISSUES[0], BIG] as never } });
    const reviewer = screen.getByRole("region", { name: "Revisor" });
    const items = within(reviewer).getAllByRole("listitem");
    expect(items).toHaveLength(2);
    expect(items[0]?.getAttribute("data-plan-too-large")).toBe("T01");
    expect(within(items[0] as HTMLElement).getByText("A T01 foi apontada como grande demais.").tagName).toBe("STRONG");
    expect(within(items[0] as HTMLElement).getByText(`Sugestão de divisão: ${BIG.text}`)).toBeTruthy();
    expect(within(items[1] as HTMLElement).getByText("T02")).toBeTruthy();
    // A tarefa ganha a marca na lista do plano.
    const task = screen.getByText("Base").closest("li") as HTMLElement;
    expect(within(task).getByText("grande demais").hasAttribute("data-task-too-large")).toBe(true);
    expect(approveButton().disabled).toBe(false);
  });

  it("sem too_large, nada de destaque nem marca", () => {
    renderWith({ review: { status: "ok", issues: ISSUES } });
    expect(document.querySelector("[data-plan-too-large]")).toBeNull();
    expect(document.querySelector("[data-task-too-large]")).toBeNull();
    expect(tooLargeIssues([...ISSUES, { task: null, kind: "too_large", text: "x" }])).toEqual([]);
  });

  it("em inglês, o destaque segue o idioma", () => {
    setLanguage("en", { persist: false });
    renderWith({ review: { status: "ok", issues: [BIG] } });
    const item = document.querySelector("[data-plan-too-large]") as HTMLElement;
    expect(within(item).getByText("T01 was flagged as too large.")).toBeTruthy();
    expect(within(item).getByText(`Suggested split: ${BIG.text}`)).toBeTruthy();
    expect(screen.getByText("too large")).toBeTruthy();
  });
});
