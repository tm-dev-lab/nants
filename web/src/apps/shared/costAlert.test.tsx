/*
 * Aviso de custo por tarefa (spec 124 T06): o texto nos dois idiomas, só das tarefas abertas na
 * Atividade, o selo discreto no cartão do Quadro, e nada de botão ou bloqueio (é só um sinal).
 */
import { act, cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CostAlertView, ProjectState, SpecView } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { costAlertText, openCostAlerts } from "./costAlert";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  appStore.setState({ activities: {} });
  setLanguage("pt-BR", { persist: false });
});

const RELATIVE: CostAlertView = { tokens: 16_290_000, calls: 637, basis: "relative", ratio: 5, median_tokens: 3_000_000, median_calls: 120 };
const ABSOLUTE: CostAlertView = { tokens: 9_000_000, calls: 1, basis: "absolute" };

function spec(): SpecView {
  const tasks = [makeTask("T01", "Base", "DONE"), makeTask("T02", "Cart", "IN_PROGRESS"), makeTask("T03", "Payment", "TODO")];
  return { ...makeSpec("s1", "Online shop", "IN_PROGRESS", tasks), cost_alerts: { T01: ABSOLUTE, T02: RELATIVE } };
}

function state(): ProjectState {
  return makeProjectState({ id: "p1", name: "Shop", specs: [spec()], running: true, run: { spec_id: "s1", status: "running" } });
}

async function mount(lang: "pt-BR" | "en") {
  setLanguage(lang, { persist: false });
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Shop")], states: { p1: state() } });
  view.server.reply("GET", /\/api\/projects\/p1\/specs\/s1\/tools/, () => ({ status: 200, body: { items: [], total: 0, has_more: false } }));
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return view;
}

describe("texto do aviso de custo", () => {
  it("relativo e absoluto, nos dois idiomas, com o plural das chamadas", () => {
    expect(costAlertText("T02", RELATIVE)).toBe("T02: 16,3M tokens e 637 chamadas, 5 vezes a mediana das tarefas concluídas. É só um aviso: a execução continua.");
    expect(costAlertText("T01", ABSOLUTE)).toBe("T01: 9,0M tokens e 1 chamada, acima do patamar fixo de custo. É só um aviso: a execução continua.");
    setLanguage("en", { persist: false });
    expect(costAlertText("T02", RELATIVE)).toBe("T02: 16.3M tokens and 637 calls, 5 times the median of the finished tasks. Only a notice: the run goes on.");
    expect(costAlertText("T01", ABSOLUTE)).toBe("T01: 9.0M tokens and 1 call, above the fixed cost threshold. Only a notice: the run goes on.");
    // Relativo sem a razão (servidor antigo ou dado estranho) cai no texto do patamar fixo.
    expect(costAlertText("T02", { tokens: 10, calls: 2, basis: "relative" })).toContain("above the fixed cost threshold");
  });

  it("a Atividade lista só as tarefas ainda abertas, na ordem do plano", () => {
    expect(openCostAlerts(spec()).map((a) => a.task)).toEqual(["T02"]);
    expect(openCostAlerts(null)).toEqual([]);
    expect(openCostAlerts(makeSpec("s1", "x", "IN_PROGRESS", [makeTask("T01", "Base", "TODO")]))).toEqual([]);
  });
});

describe("Quadro e Atividade com aviso de custo", () => {
  it("pt-BR: selo discreto no cartão e linha na Atividade, sem botão", async () => {
    await mount("pt-BR");
    const board = screen.getByRole("region", { name: "Quadro" });
    const badge = board.querySelector('[data-cost-alert="T02"]') as HTMLElement;
    expect(badge.textContent).toContain("custo alto");
    expect(badge.getAttribute("title")).toContain("5 vezes a mediana");
    expect(badge.getAttribute("data-tone")).toBe("amber");
    expect(board.querySelector('[data-cost-alert="T01"]')?.textContent).toContain("custo alto");
    expect(board.querySelector('[data-cost-alert="T03"]')).toBeNull();
    const activity = screen.getByRole("region", { name: "Atividade" });
    const section = screen.getByRole("region", { name: "Aviso de custo" });
    expect(activity.contains(section)).toBe(true);
    expect(section.querySelectorAll("li").length).toBe(1);
    expect(section.textContent).toContain("T02: 16,3M tokens e 637 chamadas");
    expect(section.querySelector("button")).toBeNull();
  });

  it("en: nada em português no cartão nem na Atividade", async () => {
    await mount("en");
    const board = screen.getByRole("region", { name: "Board" });
    const card = board.querySelector('[data-task-id="T02"]') as HTMLElement;
    expect(card.querySelector("[data-cost-alert]")?.textContent).toContain("high cost");
    expect(portugueseLeftovers(card)).toEqual([]);
    const section = screen.getByRole("region", { name: "Cost notice" });
    expect(section.textContent).toBe("Cost noticeT02: 16.3M tokens and 637 calls, 5 times the median of the finished tasks. Only a notice: the run goes on.");
    expect(portugueseLeftovers(section)).toEqual([]);
  });
});
