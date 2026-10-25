/*
 * Quadro nos dois idiomas (spec 102, T03): em en, nenhum texto visível ou acessível da janela fica
 * em português (o que os agentes escrevem aparece como veio); pt-BR igual a hoje; o subtítulo, o
 * selo e o vazio seguem o idioma; a troca vale na hora, sem recarregar.
 */
import { parseReviewMessage } from "../../reviewMessage";
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState, SpecView, TaskView, UsageView } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore, recoveryKey } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { boardSummary, lastReviewLine, recoveryLabel, toolActionsLabel } from "./boardModel";
import { formatClock } from "./ToolActionsSheet";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  appStore.setState({ activities: {}, recoveries: {} });
  setLanguage("pt-BR", { persist: false });
});

const P1 = makeProject("p1", "Shop");

/** Tarefas com conteúdo do Arquiteto em inglês (conteúdo não é traduzido). */
const TASKS = (): TaskView[] => [
  makeTask("T01", "Base", "DONE"),
  makeTask("T02", "Cart", "IN_PROGRESS", { attempts: 2, max_attempts: 3, depends_on: ["T01"], last_review_verdict: "CHANGES_REQUESTED" }),
  makeTask("T03", "Payment", "APPROVED", { last_review_verdict: "APPROVED" }),
  makeTask("T04", "Shipping", "BLOCKED", { next_action: "Waiting for the API key" }),
];

const USAGE: UsageView = {
  specs: [
    {
      spec: "s1",
      calls: 3,
      input: 100,
      output: 50,
      cached: 0,
      reasoning: 0,
      cost_usd: 0.07,
      unpriced_calls: 0,
      agents: [],
      tasks: [{ id: "T02", calls: 2, input: 80, output: 40, cached: 0, reasoning: 0, cost_usd: 1234.5, unpriced_calls: 0, active_secs: 12 * 60 }],
    },
  ],
  total: { calls: 3, input: 100, output: 50, cached: 0, reasoning: 0, cost_usd: 0.07, unpriced_calls: 0 },
  budget: null,
};

function spec(tasks: TaskView[] = TASKS()): SpecView {
  return { ...makeSpec("s1", "Online shop", "IN_PROGRESS", tasks), tool_counts: { T02: 2, FINAL: 1 } };
}

function state(s: SpecView | null): ProjectState {
  return makeProjectState({ id: "p1", name: "Shop", specs: s === null ? [] : [s], running: s !== null, run: s === null ? null : { spec_id: "s1", status: "running" } });
}

async function mount(lang: "pt-BR" | "en", s: SpecView | null = spec()) {
  setLanguage(lang, { persist: false });
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(s) } });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: USAGE }));
  view.server.reply("GET", /\/api\/projects\/p1\/specs\/s1\/tools/, () => ({
    status: 200,
    body: {
      items: [
        { seq: 1, ts: 1_700_000_000, agent: "developer", task_id: "T02", kind: "tool", text: "write src/cart.ts" },
        { seq: 2, ts: 1_700_000_005, agent: "reviewer", task_id: "T02", kind: "command", text: "npm test" },
      ],
      total: 3,
      has_more: true,
    },
  }));
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return view;
}

function board(lang: "pt-BR" | "en"): HTMLElement {
  return screen.getByRole("region", { name: lang === "en" ? "Board" : "Quadro" });
}

function columnLabels(win: HTMLElement): (string | null)[] {
  return within(win)
    .getAllByRole("group")
    .filter((g) => g.hasAttribute("data-column"))
    .map((g) => g.getAttribute("aria-label"));
}

function openBoard(): Promise<void> {
  return act(async () => {
    const dock = screen.getByRole("navigation", { name: "Dock" });
    const button = within(dock).queryByRole("button", { name: "Board" }) ?? within(dock).getByRole("button", { name: "Quadro" });
    fireEvent.click(button);
  });
}

async function addReview(): Promise<void> {
  await act(async () => {
    appStore.appendMessage("p1", { role: "agent", agent: "reviewer", taskId: "T02", specId: "s1", text: "Pediu ajustes: handle the empty cart" });
  });
}

describe("Quadro em en", () => {
  it("colunas, cartões, selos, recuperação, última revisão e subtítulo sem português", async () => {
    await mount("en");
    await act(async () => {
      appStore.setState({ recoveries: { [recoveryKey("p1", "s1", "T02")]: { phase: "analyzing", cycle: 2 } } as never });
    });
    await addReview();
    const win = board("en");
    expect(columnLabels(win)).toEqual([
      "To do, 0 tasks",
      "In development, 1 task",
      "In review, 1 task",
      "Done, 1 task",
      "Blocked, 1 task",
    ]);
    expect(within(win).getByRole("region", { name: "Task board" })).toBeTruthy();
    expect(within(win).getByRole("region", { name: "Tasks: To do" }).textContent).toContain("No tasks");
    const card = win.querySelector('[data-task-id="T02"]') as HTMLElement;
    expect(card.textContent).toContain("attempt 2 of 3");
    expect(card.textContent).toContain("depends on T01");
    expect(card.textContent).toContain("Review: Asked for changes");
    expect(card.querySelector("[data-verdict]")?.getAttribute("title")).toBe("Review asked for changes");
    expect(card.querySelector("[data-recovery]")?.textContent).toBe("Architect analyzing (cycle 2)");
    // Moeda e número pelos formatadores no idioma ativo.
    await waitFor(() => expect(win.querySelector('[data-task-usage="T02"]')?.textContent).toBe("12 min · US$ 1,234.50"));
    const approved = win.querySelector('[data-task-id="T03"]') as HTMLElement;
    expect(approved.querySelector('[data-real-status="APPROVED"]')?.textContent).toBe("Approved");
    expect(approved.querySelector("[data-verdict]")?.getAttribute("title")).toBe("Review approved");
    const blocked = win.querySelector('[data-task-id="T04"]') as HTMLElement;
    expect(blocked.textContent).toContain("Reason");
    // Conteúdo do agente: como foi escrito.
    expect(blocked.textContent).toContain("Waiting for the API key");
    expect(within(win).getByRole("button", { name: "Actions (2) of task T02" })).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Actions (1) of the final verification" })).toBeTruthy();
    expect(win.querySelector("[data-final-actions]")?.textContent).toContain("Final verification");
    const review = within(win).getByRole("region", { name: "Last review · T02" });
    expect(within(review).getByRole("button", { name: "Last review · T02 · Asked for changes" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("board"))?.textContent).toBe("4 tasks · 1 done");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("menu Colunas e folha de ações sem português", async () => {
    await mount("en");
    const win = board("en");
    fireEvent.click(within(win).getByRole("button", { name: "Columns" }));
    const popover = within(win).getByRole("dialog", { name: "Board columns" });
    expect(within(popover).getByLabelText(/Changes requested/)).toBeTruthy();
    expect(popover.textContent).toContain("(a task is blocked)");
    expect(portugueseLeftovers(win)).toEqual([]);
    fireEvent.keyDown(popover, { key: "Escape" });
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Actions (2) of task T02" }));
    });
    await settle();
    const sheet = within(win).getByRole("dialog", { name: "Task T02 actions" });
    await waitFor(() => expect(within(sheet).getByText("2 of 3 actions")).toBeTruthy());
    expect(within(sheet).getByRole("button", { name: "Close" })).toBeTruthy();
    expect(within(sheet).getByRole("button", { name: "Load earlier" })).toBeTruthy();
    expect(within(sheet).getByRole("list", { name: "Actions in chronological order" }).textContent).toContain("Reviewer");
    expect(portugueseLeftovers(sheet)).toEqual([]);
  });

  it("estado vazio, subtítulo e selo em en", async () => {
    await mount("en", null);
    await openBoard();
    await settle();
    const win = board("en");
    expect(within(win).getByText("No tasks yet. They show up here once the plan is approved.")).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Open the Plan" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("board"))?.textContent).toBe("no tasks");
    expect(portugueseLeftovers(win)).toEqual([]);
    expect(boardSummary({ space: { activeSpec: spec() } } as never)).toEqual({
      subtitle: "4 tasks · 1 done",
      badge: { count: 1, label: "1 blocked task" },
    });
  });

  it("funções do modelo no idioma ativo", () => {
    setLanguage("en", { persist: false });
    expect(toolActionsLabel(3)).toBe("Actions (3)");
    expect(recoveryLabel({ phase: "applied", cycle: 1 } as never)).toBe("New strategy applied (cycle 1)");
    expect(lastReviewLine("T02", "approved")).toBe("Last review · T02 · Approved");
    const tracked = parseReviewMessage("Pediu ajustes: x\n\nApontamentos:\n- F-02 [HIGH, aberto há 4 revisões] a:1: d\n\nResolvidos:\n- F-01 [HIGH] c:1: f\n- F-03 [HIGH] c:2: g");
    if (tracked !== null) expect(lastReviewLine("T02", tracked.verdict, tracked)).toBe("Last review · T02 · Asked for changes · 1 still open · 2 resolved");
    // Hora sempre em 24 h, nos dois idiomas.
    const at = new Date(2026, 9, 21, 18, 5, 9).getTime() / 1000;
    expect(formatClock(at)).toBe("18:05:09");
    setLanguage("pt-BR", { persist: false });
    expect(formatClock(at)).toBe("18:05:09");
    expect(lastReviewLine("T02", "changes")).toBe("Última revisão · T02 · Pediu ajustes");
  });
});

describe("Quadro em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora", async () => {
    await mount("pt-BR");
    let win = board("pt-BR");
    expect(columnLabels(win)).toEqual(["A fazer, 0 tarefas", "Em desenvolvimento, 1 tarefa", "Em revisão, 1 tarefa", "Concluídas, 1 tarefa", "Bloqueada, 1 tarefa"]);
    expect(within(win).getByRole("button", { name: "Ações (2) da tarefa T02" })).toBeTruthy();
    expect((win.querySelector('[data-task-id="T02"]') as HTMLElement).textContent).toContain("tentativa 2 de 3");
    expect(document.getElementById(windowSubtitleId("board"))?.textContent).toBe("4 tarefas · 1 concluída");
    await waitFor(() => expect(win.querySelector('[data-task-usage="T02"]')?.textContent).toBe("12 min · US$ 1.234,50"));
    expect(portugueseLeftovers(win).length).toBeGreaterThan(0);

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    win = board("en");
    expect(columnLabels(win)).toEqual(["To do, 0 tasks", "In development, 1 task", "In review, 1 task", "Done, 1 task", "Blocked, 1 task"]);
    expect(within(win).getByRole("button", { name: "Actions (2) of task T02" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("board"))?.textContent).toBe("4 tasks · 1 done");
    expect(win.querySelector('[data-task-usage="T02"]')?.textContent).toBe("12 min · US$ 1,234.50");
    expect(portugueseLeftovers(win)).toEqual([]);
  });
});

describe("Quadro em en: tarefa pausada (spec 114 T04)", () => {
  it("selo, motivo curto e a ação na conversa sem português", async () => {
    setLanguage("en", { persist: false });
    const s = { ...makeSpec("s1", "Online shop", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Cart", "IN_PROGRESS")]) };
    const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Shop", specs: [s], running: false, run: null }) } });
    view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: USAGE }));
    await act(async () => {
      appStore.appendMessage("p1", { role: "agent", agent: "system", specId: "s1", text: "O provedor do modelo falhou (x).", code: "run.provider_failed", params: { provider: "x" } });
    });
    await settle();
    const win = screen.getByRole("region", { name: "Board" });
    const card = win.querySelector('[data-task-id="T02"]') as HTMLElement;
    expect(card.querySelector('[data-paused="provider"]')?.textContent).toBe("Pausedthe model provider failed");
    expect(within(win).getByRole("button", { name: "Continue in the conversation: task T02" })).toBeTruthy();
    expect(portugueseLeftovers(win)).toEqual([]);
  });
});
