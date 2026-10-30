/*
 * Janela Quadro montada pelo shell (spec 093, T07): colunas e cartões (tempo e custo por tarefa),
 * as colunas no ui-state, a folha de ações dentro da janela, a última revisão e o `data-size`.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState, SpecView, TaskView, UsageView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, openNMenu, renderShell, settle } from "../../desktop/shell-test-utils";
import { fakeResizeObservers } from "../../desktop/test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { appStore, recoveryKey } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  // O `resetStore` não limpa as atividades ao vivo.
  appStore.setState({ activities: {} });
});

const P1 = makeProject("p1", "Loja");

function spec(tasks: TaskView[], extra: Partial<SpecView> = {}): SpecView {
  return { ...makeSpec("s1", "Loja online", "IN_PROGRESS", tasks), ...extra };
}

function running(s: SpecView): ProjectState {
  return makeProjectState({ id: "p1", name: "Loja", specs: [s], running: true, run: { spec_id: "s1", status: "running" } });
}

const TASKS = (): TaskView[] => [
  makeTask("T01", "Base", "DONE"),
  makeTask("T02", "Carrinho", "IN_PROGRESS", { attempts: 2, max_attempts: 3, depends_on: ["T01"], last_review_verdict: "CHANGES_REQUESTED" }),
  makeTask("T03", "Pagamento", "APPROVED"),
  makeTask("T04", "Frete", "TODO", { depends_on: ["T02", "T03"] }),
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
      tasks: [{ id: "T02", calls: 2, input: 80, output: 40, cached: 0, reasoning: 0, cost_usd: 0.04, unpriced_calls: 0, active_secs: 12 * 60 }],
    },
  ],
  total: { calls: 3, input: 100, output: 50, cached: 0, reasoning: 0, cost_usd: 0.07, unpriced_calls: 0 },
  budget: null,
};

function board(): HTMLElement {
  return screen.getByRole("region", { name: "Quadro" });
}

async function mount(s: SpecView, extra: Parameters<typeof renderShell>[0] = {}) {
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: running(s) }, ...extra });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: USAGE }));
  view.server.reply("GET", /\/api\/projects\/p1\/specs\/s1\/tools/, (_b, url) => ({
    status: 200,
    body: {
      items: [
        { seq: 1, ts: 1_700_000_000, agent: "developer", task_id: url.includes("FINAL") ? "FINAL" : "T02", kind: "tool", text: url.includes("FINAL") ? "conferindo a spec" : "escrevendo src/cart.ts" },
        { seq: 2, ts: 1_700_000_005, agent: "developer", task_id: "T02", kind: "command", text: "npm test" },
      ],
      total: 2,
      has_more: false,
    },
  }));
  // Relê o estado (e o uso) com as rotas novas.
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return view;
}

describe("Quadro: colunas, cartões e resumo", () => {
  it("colunas padrão com contagem, cartão com tentativa, veredito, dono, dependências, tempo e custo", async () => {
    await mount(spec(TASKS()));
    const win = board();
    const groups = within(win).getAllByRole("group").filter((g) => g.hasAttribute("data-column"));
    expect(groups.map((g) => g.getAttribute("aria-label"))).toEqual([
      "A fazer, 1 tarefa",
      "Em desenvolvimento, 1 tarefa",
      "Em revisão, 1 tarefa",
      "Concluídas, 1 tarefa",
    ]);
    const card = win.querySelector('[data-task-id="T02"]') as HTMLElement;
    expect(card.textContent).toContain("tentativa 2 de 3");
    expect(card.textContent).toContain("depende de T01");
    expect(card.textContent).toContain("Pediu ajustes");
    expect(card.querySelector("[data-avatar-ring]") ?? card.querySelector(".nd-avatar")).not.toBeNull();
    await waitFor(() => expect(win.querySelector('[data-task-usage="T02"]')?.textContent).toBe("12 min · US$ 0,04"));
    // Aprovada (coluna oculta) aparece em "Em revisão" com o selo do estado real.
    const approved = win.querySelector('[data-column="IN_REVIEW"] [data-task-id="T03"]') as HTMLElement;
    expect(approved.querySelector('[data-real-status="APPROVED"]')?.textContent).toBe("Aprovada");
    expect(document.getElementById(windowSubtitleId("board"))?.textContent).toBe("4 tarefas · 1 concluída");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("tarefa bloqueada: coluna Bloqueada com o motivo e selo no menu do N; selo de recuperação no cartão", async () => {
    const tasks = [makeTask("T01", "Base", "BLOCKED", { next_action: "o teste falha" }), makeTask("T02", "Carrinho", "IN_PROGRESS")];
    appStore.setState({});
    await mount(spec(tasks));
    await act(async () => {
      appStore.setState({ recoveries: { [recoveryKey("p1", "s1", "T02")]: { phase: "analyzing", cycle: 2 } } });
    });
    const win = board();
    const blocked = win.querySelector('[data-column="BLOCKED"]') as HTMLElement;
    expect(blocked.textContent).toContain("Motivo");
    expect(blocked.textContent).toContain("o teste falha");
    expect(win.querySelector('[data-recovery="analyzing"]')?.textContent).toBe("Arquiteto analisando (ciclo 2)");
    // O selo do Quadro aparece no menu do N.
    const menu = openNMenu();
    expect(menu.querySelector('[data-kind="board"] [data-badge]')?.textContent ?? "").toContain("1");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("execução parada pelo provedor: a tarefa aparece pausada, com o motivo e o acesso à conversa; ao continuar, volta a Em desenvolvimento (spec 114 T04)", async () => {
    const s = spec([makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS"), makeTask("T03", "Pagamento", "TODO")]);
    const stopped = makeProjectState({ id: "p1", name: "Loja", specs: [s], running: false, run: null });
    const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stopped } });
    view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: USAGE }));
    await act(async () => {
      appStore.appendMessage("p1", {
        role: "agent",
        agent: "system",
        specId: "s1",
        text: "O provedor do modelo falhou (provider_bad_response: too_large).",
        code: "run.provider_failed_detail",
        params: { provider: "provider_bad_response", reason: "too_large" },
      });
    });
    const win = board();
    const card = win.querySelector('[data-column="IN_PROGRESS"] [data-task-id="T02"]') as HTMLElement;
    const pause = card.querySelector('[data-paused="provider"]') as HTMLElement;
    expect(pause.textContent).toBe("Pausadao provedor do modelo falhou");
    // Sem pulsar "pensando": ninguém está trabalhando na tarefa.
    expect(card.querySelector('[data-avatar-state="thinking"]')).toBeNull();
    expect(win.querySelector('[data-task-id="T03"]')?.closest("li")?.querySelector("[data-paused]")).toBeNull();
    const action = within(win).getByRole("button", { name: "Continuar na conversa: tarefa T02" });
    expect(action.textContent).toBe("Continuar na conversa");
    await act(async () => {
      fireEvent.click(action);
    });
    await settle();
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
    // Continuou: o run voltou e a tarefa volta a "Em desenvolvimento", sem o selo.
    view.server.model.states.p1 = running(s);
    await act(async () => {
      appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    const resumed = board().querySelector('[data-column="IN_PROGRESS"] [data-task-id="T02"]') as HTMLElement;
    expect(resumed.querySelector("[data-paused]")).toBeNull();
    expect(board().querySelector("[data-pause-action]")).toBeNull();
    expect(findForbiddenGlyphs(board())).toEqual([]);
  });

  it("pausa por limite de conta: o motivo diz o papel que parou (spec 133 T02)", async () => {
    const s = spec([makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS")]);
    const stopped = makeProjectState({ id: "p1", name: "Loja", specs: [s], running: false, run: null });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stopped } });
    await act(async () => {
      appStore.appendMessage("p1", {
        role: "agent",
        agent: "system",
        specId: "s1",
        text: "provider_billing",
        code: "run.billing",
        params: { role: "developer", task: "T02", connection_id: "anthropic", connection_name: "Claude", connection_kind: "anthropic", model: "claude-x", category: "billing", status: 402 },
      });
    });
    const pause = board().querySelector('[data-task-id="T02"] [data-paused="billing"]') as HTMLElement;
    expect(pause.textContent).toBe("PausadaDesenvolvedor: sem crédito no provedor");
    setLanguage("en", { persist: false });
    await settle();
    expect(document.querySelector('[data-task-id="T02"] [data-paused="billing"]')?.textContent).toContain("Developer: out of credit at the provider");
    setLanguage("pt-BR", { persist: false });
  });

  it("parada antiga, retomada e queda sem mensagem: mostra 'execução interrompida', não o motivo antigo", async () => {
    const s = spec([makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS")]);
    const stopped = makeProjectState({ id: "p1", name: "Loja", specs: [s], running: false, run: null });
    const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stopped } });
    await act(async () => {
      appStore.appendMessage("p1", { role: "agent", agent: "system", specId: "s1", text: "O provedor do modelo falhou (x).", code: "run.provider_failed", params: { provider: "x" } });
    });
    expect(board().querySelector('[data-paused="provider"]')).not.toBeNull();
    // Retomada: o servidor avisa que voltou a executar.
    view.server.model.states.p1 = running(s);
    await act(async () => {
      view.stream().emit("project_status", { event_type: "project_status", project_id: "p1", payload: { status: "executing", spec_id: "s1" } });
    });
    await act(async () => {
      appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    expect(board().querySelector("[data-paused]")).toBeNull();
    // Queda sem mensagem de parada (servidor reiniciado): o motivo antigo não volta.
    view.server.model.states.p1 = stopped;
    await act(async () => {
      view.stream().emit("project_status", { event_type: "project_status", project_id: "p1", payload: { status: "idle", reason: "shutdown_forced" } });
    });
    await act(async () => {
      appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    expect(board().querySelector('[data-paused="interrupted"]')?.textContent).toBe("Pausadaexecução interrompida");
  });

  it("parada pela pessoa: motivo curto próprio", async () => {
    const s = spec([makeTask("T02", "Carrinho", "IN_REVIEW")]);
    const stopped = makeProjectState({ id: "p1", name: "Loja", specs: [s], running: false, run: null });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: stopped } });
    await act(async () => {
      appStore.appendMessage("p1", { role: "agent", agent: "system", specId: "s1", text: "Execução parada pelo usuário.", code: "run.stopped" });
    });
    expect(board().querySelector('[data-paused="stopped"]')?.textContent).toBe("Pausadaparada por você");
  });

  it("pedido de permissão aguardando a pessoa: 'Aguardando você' e 'Decidir na conversa'", async () => {
    const s = spec([makeTask("T02", "Carrinho", "IN_REVIEW")]);
    const waiting = makeProjectState({
      id: "p1",
      name: "Loja",
      specs: [s],
      running: true,
      run: { spec_id: "s1", status: "running" },
      pending_permissions: [{ request_id: "r1", agent: "reviewer", command: "npm test", task_id: "T02" }],
    });
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: waiting } });
    await settle();
    // Com pedido pendente, o layout abre a Conversa e o Plano: o Quadro vem pelo menu do N.
    await act(async () => {
      activateApp(/Quadro/);
    });
    await settle();
    expect(board().querySelector('[data-paused="decision"]')?.textContent).toBe("Aguardando vocêaguardando sua decisão");
    expect(within(board()).getByRole("button", { name: "Decidir na conversa: tarefa T02" })).toBeTruthy();
  });

  it("sem tarefas: estado vazio com 'Abrir o Plano'", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec([])], running: true, run: { spec_id: "s1", status: "running" } }) },
    });
    const win = board();
    expect(win.textContent).toContain("Ainda não há tarefas.");
    fireEvent.click(within(win).getByRole("button", { name: "Abrir o Plano" }));
    await settle();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
  });
});

describe("Quadro: colunas configuráveis no ui-state", () => {
  it("'Colunas' mostra a coluna escolhida e grava em selection.boardColumns (sem storage próprio)", async () => {
    const setItem = vi.spyOn(Storage.prototype, "setItem");
    const { server } = await mount(spec(TASKS()));
    const win = board();
    fireEvent.click(within(win).getByRole("button", { name: "Colunas" }));
    const popover = within(win).getByRole("dialog", { name: "Colunas do quadro" });
    await act(async () => {
      fireEvent.click(within(popover).getByRole("checkbox", { name: "Aprovada" }));
    });
    await settle();
    expect(win.querySelector('[data-column="APPROVED"] [data-task-id="T03"]')).not.toBeNull();
    const put = server.calls.find((c) => c.method === "PUT" && (c.body as { section?: string }).section === "selection");
    expect((put?.body as { value: { boardColumns: string[] } }).value.boardColumns).toEqual(["APPROVED"]);
    // As colunas vão só para o ui-state: nada além do cache dele no storage.
    expect(setItem.mock.calls.every(([key]) => String(key).startsWith("nants.ui-state."))).toBe(true);
  });

  it("lê as colunas salvas no ui-state; Bloqueada fica marcada e travada com tarefa bloqueada", async () => {
    await mount(spec([...TASKS(), makeTask("T05", "Erro", "BLOCKED")]), {
      ui: { p1: { rev: 1, data: { selection: { boardColumns: ["INTEGRATING"] } } } },
    });
    const win = board();
    expect(win.querySelector('[data-column="INTEGRATING"]')).not.toBeNull();
    fireEvent.click(within(win).getByRole("button", { name: "Colunas" }));
    const blocked = within(win).getByRole("checkbox", { name: /Bloqueada/ }) as HTMLInputElement;
    expect(blocked.checked).toBe(true);
    expect(blocked.disabled).toBe(true);
    // Esc fecha e devolve o foco ao botão.
    fireEvent.keyDown(blocked, { key: "Escape" });
    expect(within(win).queryByRole("dialog", { name: "Colunas do quadro" })).toBeNull();
    expect(document.activeElement).toBe(within(win).getByRole("button", { name: "Colunas" }));
  });
});

describe("Quadro: seleção, teclado e troca de coluna", () => {
  it("clique marca o cartão em foco (aria-pressed) e grava a tarefa em selection.taskId", async () => {
    const { server } = await mount(spec(TASKS()));
    const win = board();
    // Padrão: a tarefa ativa.
    expect(win.querySelector('[data-task-id="T02"]')?.getAttribute("aria-pressed")).toBe("true");
    await act(async () => {
      fireEvent.click(win.querySelector('[data-task-id="T04"]') as HTMLElement);
    });
    await settle();
    expect(win.querySelector('[data-task-id="T04"]')?.getAttribute("aria-pressed")).toBe("true");
    const puts = server.calls.filter((c) => c.method === "PUT" && (c.body as { section?: string }).section === "selection");
    expect((puts.at(-1)?.body as { value: { taskId: string } }).value.taskId).toBe("T04");
  });

  it("setas navegam entre cartões da coluna e entre colunas", async () => {
    await mount(spec([makeTask("T01", "a", "TODO"), makeTask("T02", "b", "TODO"), makeTask("T03", "c", "IN_PROGRESS")]));
    const win = board();
    const t1 = win.querySelector('[data-task-id="T01"]') as HTMLElement;
    t1.focus();
    fireEvent.keyDown(t1, { key: "ArrowDown" });
    expect(document.activeElement?.getAttribute("data-task-id")).toBe("T02");
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: "ArrowRight" });
    expect(document.activeElement?.getAttribute("data-task-id")).toBe("T03");
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: "ArrowLeft" });
    expect(document.activeElement?.getAttribute("data-task-id")).toBe("T01");
  });

  it("ao trocar de coluna, só o cartão que mudou anima e o foco volta a ele", async () => {
    const { server } = await mount(spec([makeTask("T01", "a", "TODO"), makeTask("T02", "b", "TODO")]));
    const win = board();
    const t1 = win.querySelector('[data-task-id="T01"]') as HTMLElement;
    fireEvent.focusIn(t1);
    t1.focus();
    expect(win.querySelector('[data-enter="true"]')).toBeNull();
    server.model.states.p1 = running(spec([makeTask("T01", "a", "IN_PROGRESS"), makeTask("T02", "b", "TODO")]));
    await act(async () => {
      appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    const moved = win.querySelector('[data-column="IN_PROGRESS"] [data-task-id="T01"]') as HTMLElement;
    expect(moved.closest("li")?.getAttribute("data-enter")).toBe("true");
    expect(win.querySelector('[data-task-id="T02"]')?.closest("li")?.getAttribute("data-enter")).toBeNull();
    expect(document.activeElement).toBe(moved);
  });
});

describe("Quadro: Ações (N) e a verificação final", () => {
  it("'Ações (N)' abre a folha DENTRO da janela, com a trilha; Esc fecha e devolve o foco ao link", async () => {
    await mount(spec(TASKS(), { tool_counts: { T02: 2, FINAL: 1 } }));
    const win = board();
    const link = within(win).getByRole("button", { name: "Ações (2) da tarefa T02" });
    await act(async () => {
      fireEvent.click(link);
    });
    await settle();
    const sheet = within(win).getByRole("dialog", { name: "Ações da tarefa T02" });
    expect(sheet.textContent).toContain("escrevendo src/cart.ts");
    expect(sheet.textContent).toContain("npm test");
    expect(sheet.textContent).toContain("2 de 2 ações");
    // O quadro por baixo fica inerte enquanto a folha está aberta.
    expect(win.querySelector(".nd-board-main")?.hasAttribute("inert")).toBe(true);
    expect(findForbiddenGlyphs(sheet)).toEqual([]);
    await act(async () => {
      fireEvent.keyDown(sheet, { key: "Escape" });
      await new Promise((r) => setTimeout(r, 30));
    });
    expect(within(win).queryByRole("dialog", { name: "Ações da tarefa T02" })).toBeNull();
    expect(document.activeElement).toBe(within(win).getByRole("button", { name: "Ações (2) da tarefa T02" }));
  });

  it("'Ações da verificação final' abre a trilha FINAL; sem ações, não há link", async () => {
    const { server } = await mount(spec(TASKS(), { tool_counts: { FINAL: 3 } }));
    const win = board();
    expect(within(win).queryByRole("button", { name: /da tarefa/ })).toBeNull();
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Ações (3) da verificação final" }));
    });
    await settle();
    const sheet = within(win).getByRole("dialog", { name: "Ações da verificação final" });
    expect(sheet.textContent).toContain("conferindo a spec");
    expect(server.calls.some((c) => c.url.includes("/tools?task=FINAL"))).toBe(true);
    fireEvent.click(within(sheet).getByRole("button", { name: "Fechar" }));
    expect(within(win).queryByRole("dialog")).toBeNull();
  });

  it("verificação final em andamento aparece em destaque no Quadro", async () => {
    await mount(spec(TASKS()));
    await act(async () => {
      appStore.applyEvent({
        event_type: "agent_activity",
        project_id: "p1",
        payload: { agent: "reviewer", activity_id: "f1", kind: "start", stage: "final_verification" },
      } as never);
    });
    expect(within(board()).getByTestId("final-verification-strip").textContent).toContain("Verificação final em andamento");
  });
});

const REVIEW_TEXT =
  "Pediu ajustes — falta tratar o erro\n\nApontamentos:\n- F-01 [HIGH] src/cart.ts:3 — sem validação\n\n" +
  "Observações (não bloqueiam):\n- F-07 [MEDIUM, observação (arquivo não mudou nesta rodada)] src/b.ts:3 — nome confuso\n\n" +
  "Critérios:\n✓ Carrinho soma — ok\n✗ Erro tratado — falta";

async function addReview(): Promise<void> {
  await act(async () => {
    appStore.appendMessage("p1", { role: "agent", agent: "reviewer", taskId: "T02", specId: "s1", text: REVIEW_TEXT });
  });
}

/** Corpo da regra de nível do arquivo com exatamente este seletor no `board.css`. */
function boardRule(selector: string): string {
  const css = readFileSync(resolve(process.cwd(), "src/apps/board/board.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
  const index = css.indexOf(`${selector} {`);
  if (index === -1) throw new Error(`regra ausente: ${selector}`);
  const start = css.indexOf("{", index) + 1;
  return css.slice(start, css.indexOf("}", start));
}

describe("Quadro: última revisão da tarefa em foco", () => {
  it("recolhida: UMA linha 'Última revisão · T02 · Pediu ajustes'; aberta, o veredito, a observação rebaixada e a matriz", async () => {
    await mount(spec(TASKS()));
    await addReview();
    const review = within(board()).getByRole("region", { name: "Última revisão · T02" });
    const toggle = within(review).getByRole("button", { name: "Última revisão · T02 · Pediu ajustes" });
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    // Recolhida: só a linha (nenhum apontamento, veredito ou critério montado).
    expect(Array.from(review.children)).toEqual([toggle]);
    expect(within(review).queryByTestId("review-verdict")).toBeNull();
    expect(within(review).queryByRole("list")).toBeNull();
    await act(async () => {
      fireEvent.click(toggle);
    });
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    const body = review.querySelector<HTMLElement>("[data-review-body]");
    expect(body?.id).toBe(toggle.getAttribute("aria-controls"));
    expect(within(review).getByTestId("review-verdict").textContent).toContain("Pediu ajustes");
    expect(within(review).getByTestId("review-unchanged-file").textContent).toBe("observação (arquivo não mudou nesta rodada)");
    const criteria = within(review).getByRole("list", { name: "Critérios de aceitação" });
    expect(within(criteria).getAllByRole("listitem")).toHaveLength(2);
    expect(within(criteria).getByRole("img", { name: "não atendido" })).toBeTruthy();
    expect(findForbiddenGlyphs(review)).toEqual([]);
    // Recolher de novo.
    await act(async () => {
      fireEvent.click(toggle);
    });
    expect(review.querySelector("[data-review-body]")).toBeNull();
  });

  it("CSS: aberta, no máximo 40% da altura com rolagem própria; as colunas ficam com o resto em qualquer tamanho", () => {
    const section = boardRule(".nd-board-review");
    expect(section).toContain("max-height: 40%;");
    expect(section).toContain("flex-shrink: 0;");
    expect(section).not.toMatch(/(?:^|;)\s*flex:\s*1/);
    const body = boardRule(".nd-board-review-body");
    expect(body).toContain("overflow: auto;");
    expect(body).toContain("min-height: 0;");
    expect(boardRule(".nd-board-review-line")).toContain("white-space: nowrap;");
    expect(boardRule(".nd-board")).toMatch(/flex: 1;[\s\S]*min-height: 0;/);
    // Nenhuma regra (nem a da lista estreita) tira o teto de 40% da revisão.
    const css = readFileSync(resolve(process.cwd(), "src/apps/board/board.css"), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    const reviewRules = css.match(/[^{}]*\.nd-board-review[^{}]*\{[^}]*\}/g) ?? [];
    expect(reviewRules.filter((rule) => /max-height:\s*none|overflow:\s*visible/.test(rule))).toEqual([]);
  });
});

describe("Quadro: responsividade por data-size", () => {
  it("estreita: colunas em lista com a rolagem dentro da janela; ampla: colunas lado a lado", async () => {
    const observers = fakeResizeObservers();
    await mount(spec(TASKS().filter((t) => t.id !== "T03")), { createObserver: observers.factory });
    const win = board();
    const grid = win.querySelector("[data-board-layout]") as HTMLElement;
    expect(grid.getAttribute("data-board-layout")).toBe("columns");
    expect(grid.style.gridTemplateColumns).toContain("repeat(4");
    expect(win.querySelector('[data-column="IN_REVIEW"] [data-board-scroll]')).not.toBeNull();
    await act(async () => observers.resize(400));
    expect(win.getAttribute("data-size")).toBe("narrow");
    expect(grid.getAttribute("data-board-layout")).toBe("list");
    expect(grid.style.gridTemplateColumns).toBe("");
    // Coluna vazia no estreito: só o cabeçalho (sem área de rolagem); as outras continuam na lista.
    expect(win.querySelector('[data-column="IN_REVIEW"] [data-board-scroll]')).toBeNull();
    expect(win.querySelector('[data-column="TODO"] [data-board-scroll]')).not.toBeNull();
    await act(async () => observers.resize(1000));
    expect(grid.getAttribute("data-board-layout")).toBe("columns");
  });

  it("com a revisão presente, em cada data-size: a revisão recolhida é a linha sob as colunas, que continuam inteiras", async () => {
    const observers = fakeResizeObservers();
    await mount(spec(TASKS()), { createObserver: observers.factory });
    await addReview();
    const win = board();
    for (const [width, size, layout] of [
      [1000, "wide", "columns"],
      [700, "medium", "columns"],
      [400, "narrow", "list"],
    ] as const) {
      await act(async () => observers.resize(width));
      expect(win.getAttribute("data-size")).toBe(size);
      const grid = win.querySelector<HTMLElement>("[data-board-layout]");
      expect(grid?.getAttribute("data-board-layout")).toBe(layout);
      const review = within(win).getByRole("region", { name: "Última revisão · T02" });
      // A revisão vem depois das colunas (nunca no lugar delas) e começa recolhida.
      expect(grid?.parentElement).toBe(review.parentElement);
      expect(Array.from(review.parentElement?.children ?? []).indexOf(review)).toBeGreaterThan(Array.from(review.parentElement?.children ?? []).indexOf(grid as HTMLElement));
      expect(review.getAttribute("data-open")).toBe("false");
      expect(within(review).getByRole("button", { name: "Última revisão · T02 · Pediu ajustes" }).getAttribute("aria-expanded")).toBe("false");
      expect(win.querySelectorAll('[data-column="IN_PROGRESS"] [data-task-id]')).toHaveLength(1);
    }
    // Aberta no estreito: o corpo é a área com rolagem própria; as colunas continuam montadas.
    await act(async () => {
      fireEvent.click(within(win).getByRole("button", { name: "Última revisão · T02 · Pediu ajustes" }));
    });
    expect(win.querySelector('[data-last-review="T02"] [data-review-body]')?.classList.contains("nd-board-review-body")).toBe(true);
    expect(win.querySelectorAll("[data-task-id]").length).toBe(4);
  });
});

describe("Quadro: acessibilidade, teclado e foco", () => {
  it("região rotulada, colunas como grupos com contagem, listas e cartões com id e título; textos longos quebram", async () => {
    await mount(spec([makeTask("T01", "Base", "TODO", { depends_on: ["T_ID_MUITO_LONGO_SEM_ESPACOS_0123456789"] }), makeTask("T02", "Outra", "TODO")]));
    const quadro = within(board()).getByRole("region", { name: "Quadro de tarefas" });
    const todo = within(quadro).getByRole("group", { name: /^A fazer, 2 tarefas/ });
    expect(todo.querySelectorAll("ul > li").length).toBe(2);
    const card = within(todo).getByRole("button", { name: /T01.*Base/ });
    expect(card.querySelector(".nd-board-card-id")?.classList.contains("nd-break")).toBe(true);
    expect(within(card).getByText(/depende de T_ID_MUITO_LONGO/).classList.contains("nd-break")).toBe(true);
  });

  it("cartões em andamento mostram o avatar de quem está com a tarefa (pensando, decorativo); a entrada não anima com movimento reduzido", async () => {
    await mount(spec([makeTask("T01", "a", "IN_PROGRESS"), makeTask("T02", "b", "IN_REVIEW"), makeTask("T03", "c", "TODO")]));
    const dev = board().querySelector('[data-task-id="T01"] [data-kind="developer"]');
    expect(dev?.getAttribute("aria-hidden")).toBe("true");
    expect(dev?.getAttribute("data-state")).toBe("thinking");
    expect(board().querySelector('[data-task-id="T02"] [data-kind="reviewer"]')).not.toBeNull();
    expect(board().querySelector('[data-task-id="T03"] [data-kind]')).toBeNull();
    const css = readFileSync(resolve(process.cwd(), "src/apps/board/board.css"), "utf8");
    const reduced = css.slice(css.indexOf("@media (prefers-reduced-motion: reduce)"));
    expect(reduced).toContain("animation: none");
  });

  it("Enter e Espaço selecionam o cartão (aria-pressed)", async () => {
    await mount(spec([makeTask("T01", "a", "TODO"), makeTask("T02", "b", "TODO")]));
    const user = userEvent.setup();
    const t2 = board().querySelector<HTMLElement>('[data-task-id="T02"]');
    if (t2 === null) throw new Error("cartão ausente");
    t2.focus();
    await user.keyboard("{Enter}");
    await settle();
    expect(board().querySelector('[data-task-id="T02"]')?.getAttribute("aria-pressed")).toBe("true");
    (board().querySelector('[data-task-id="T01"]') as HTMLElement).focus();
    await user.keyboard(" ");
    await settle();
    expect(board().querySelector('[data-task-id="T01"]')?.getAttribute("aria-pressed")).toBe("true");
  });

  it("mudança de coluna não rouba o foco de um controle fora do quadro", async () => {
    const { server } = await mount(spec([makeTask("T01", "a", "TODO"), makeTask("T02", "b", "TODO")]));
    const t1 = board().querySelector('[data-task-id="T01"]') as HTMLElement;
    t1.focus();
    const outside = screen.getByRole("button", { name: "Personalizar" });
    fireEvent.pointerDown(outside);
    outside.focus();
    server.model.states.p1 = running(spec([makeTask("T01", "a", "IN_PROGRESS"), makeTask("T02", "b", "TODO")]));
    await act(async () => {
      appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    expect(document.activeElement).toBe(outside);
  });

  it("Colunas: abre pelo teclado com o foco no primeiro item; Esc fecha e devolve o foco ao botão", async () => {
    await mount(spec(TASKS()));
    const button = within(board()).getByRole("button", { name: "Colunas" });
    fireEvent.click(button);
    const popover = within(board()).getByRole("dialog", { name: "Colunas do quadro" });
    expect(document.activeElement).toBe(within(popover).getAllByRole("checkbox").find((c) => !(c as HTMLInputElement).disabled));
    fireEvent.keyDown(document.activeElement as HTMLElement, { key: "Escape" });
    expect(within(board()).queryByRole("dialog", { name: "Colunas do quadro" })).toBeNull();
    expect(document.activeElement).toBe(button);
  });
});

describe("Quadro: acabamento dos cartões (auditoria visual, 094/T08)", () => {
  it("A-02: metadados numa linha só com '·', sem mono (só o id); 'Ações (N)' no rodapé DENTRO do cartão", async () => {
    await mount(spec(TASKS(), { tool_counts: { T02: 2 } }));
    const win = board();
    const card = win.querySelector('[data-task-id="T02"]') as HTMLElement;
    const meta = card.querySelector("[data-card-meta]") as HTMLElement;
    expect(meta.classList.contains("nd-meta-line")).toBe(true);
    await waitFor(() => expect(meta.querySelector('[data-task-usage="T02"]')).not.toBeNull());
    const parts = Array.from(meta.children);
    expect(parts.map((p) => p.textContent)).toEqual(["tentativa 2 de 3", "·12 min · US$ 0,04", "·depende de T01"]);
    expect(meta.querySelectorAll('.nd-meta-sep[aria-hidden="true"]')).toHaveLength(2);
    // Mono só no id da tarefa.
    expect(Array.from(card.querySelectorAll(".nd-mono")).map((e) => e.textContent)).toEqual(["T02"]);
    const item = card.closest("li") as HTMLElement;
    const foot = item.querySelector("[data-card-foot]") as HTMLElement;
    expect(within(foot).getByRole("button", { name: "Ações (2) da tarefa T02" })).toBeTruthy();
    expect(item.getAttribute("data-selected")).toBe("true");
  });

  it("A-03: coluna vazia mostra 'Nenhuma tarefa', sem a caixa tracejada", async () => {
    await mount(spec([makeTask("T01", "Base", "DONE")]));
    const todo = board().querySelector('[data-column="TODO"]') as HTMLElement;
    expect(todo.querySelector("[data-column-empty]")?.textContent).toBe("Nenhuma tarefa");
    expect(board().querySelector(".nd-board-placeholder")).toBeNull();
  });

  it("A-04: selo de veredito curto, numa linha, com o rótulo completo para o leitor de tela e na dica", async () => {
    await mount(spec([makeTask("T01", "Base", "DONE", { last_review_verdict: "APPROVED" })]));
    const pill = board().querySelector('[data-verdict="APPROVED"]') as HTMLElement;
    expect(pill.classList.contains("nd-pill-nowrap")).toBe(true);
    expect(pill.getAttribute("title")).toBe("Revisão aprovada");
    expect(pill.querySelector(".nd-sr-only")?.textContent).toBe("Revisão: ");
    expect(pill.textContent).toBe("Revisão: Aprovada");
    expect(findForbiddenGlyphs(board())).toEqual([]);
  });
});

describe("Quadro enxuto (095/T05, reference/Main.dc.html)", () => {
  it("raias: --lane, raio 12, sem contorno; cabeçalho em caixa-alta caption com contagem e --divider", async () => {
    const lane = boardRule(".nd-board-column");
    expect(lane).toContain("background: var(--lane);");
    expect(lane).toContain("border-radius: var(--r-lg);");
    expect(lane).not.toMatch(/(?:^|;)\s*border(?:-color)?:/);
    const head = boardRule(".nd-board-column-head");
    expect(head).toContain("border-bottom: 1px solid var(--divider);");
    expect(head).toContain("text-transform: uppercase;");
    expect(head).toContain("font-size: var(--fs-caption);");
    await mount(spec(TASKS()));
    const todo = board().querySelector('[data-column="TODO"]') as HTMLElement;
    expect(todo.querySelector("h3")?.textContent).toBe("A fazer·1");
    expect(todo.querySelector("[data-column-count]")?.classList.contains("nd-num")).toBe(true);
    expect(todo.getAttribute("aria-label")).toBe("A fazer, 1 tarefa");
  });

  it("cartões compactos: --card-edge sem contorno, id mono, título 500, meta sans --muted, selo pequeno de raio moderado", async () => {
    const item = boardRule(".nd-board-item");
    expect(item).toContain("box-shadow: var(--card-edge);");
    expect(item).toContain("border: 1px solid transparent;");
    expect(boardRule(".nd-board-card-title")).toContain("font-weight: var(--fw-medium);");
    expect(boardRule(".nd-board-card-id")).toContain("color: var(--muted);");
    expect(boardRule(".nd-board-card .nd-pill")).toContain("border-radius: var(--r-sm);");
    // Raio cheio nunca no Quadro (decisão 9): só pontos, selos de contagem, trilho e progresso.
    expect(readFileSync(resolve(process.cwd(), "src/apps/board/board.css"), "utf8")).not.toContain("--r-pill");
    await mount(spec(TASKS(), { tool_counts: { T02: 3 } }));
    const card = board().querySelector('[data-task-id="T02"]') as HTMLElement;
    expect(card.querySelector(".nd-board-card-id.nd-mono")?.textContent).toBe("T02");
    expect(card.querySelector(".nd-board-card-title")?.textContent).toBe("Carrinho");
    expect(card.querySelectorAll("[data-card-meta]")).toHaveLength(1);
    expect(card.querySelector("[data-card-meta]")?.closest(".nd-mono")).toBeNull();
    expect(card.querySelector('[data-verdict="CHANGES_REQUESTED"]')?.classList.contains("nd-pill-sm")).toBe(true);
    const foot = (card.closest("li") as HTMLElement).querySelector("[data-card-foot]") as HTMLElement;
    expect(within(foot).getByRole("button", { name: "Ações (3) da tarefa T02" })).toBeTruthy();
  });

  it("Última revisão: faixa --lane recolhível, sem caixa de cartão", () => {
    const review = boardRule(".nd-board-review");
    expect(review).toContain("background: var(--lane);");
    expect(review).not.toMatch(/(?:^|;)\s*border:/);
  });
});

describe("Quadro: fase da tarefa durante uma operação longa (spec 123 T02)", () => {
  const activity = (payload: Record<string, unknown>) => ({ event_type: "agent_activity", project_id: "p1", payload: { activity_id: "T02-1-9", ...payload } }) as const;

  it("durante os testes, o cartão mostra 'testes rodando · 7 min' e, perto do limite, o aviso; some no fim", async () => {
    const view = await mount(spec(TASKS()));
    const card = () => board().querySelector('[data-task-id="T02"]') as HTMLElement;
    expect(card().querySelector("[data-task-phase]")).toBeNull();
    await act(async () => {
      view.stream().emit("agent_activity", activity({ agent: "tests", task_id: "T02", kind: "command", text: "npm test", command_started_at: Date.now() - 420_000, command_limit_ms: 600_000 }));
    });
    await settle();
    const phase = card().querySelector("[data-task-phase]") as HTMLElement;
    expect(phase.getAttribute("data-task-phase")).toBe("tests");
    expect(phase.textContent).toBe("testes rodando · 7 min");
    expect(phase.getAttribute("data-near-limit")).toBeNull();
    // 9 de 10 min: o aviso discreto aparece no mesmo lugar.
    await act(async () => {
      view.stream().emit("agent_activity", activity({ agent: "tests", task_id: "T02", activity_id: "T02-1-10", kind: "command", text: "npm test", command_started_at: Date.now() - 540_000, command_limit_ms: 600_000 }));
    });
    await settle();
    const near = card().querySelector("[data-task-phase]") as HTMLElement;
    expect(near.textContent).toBe("testes rodando · 9 min · perto do limite de tempo");
    expect(near.getAttribute("data-near-limit")).toBe("true");
    expect(findForbiddenGlyphs(near)).toEqual([]);
    await act(async () => {
      view.stream().emit("agent_activity", activity({ agent: "tests", task_id: "T02", activity_id: "T02-1-10", kind: "end" }));
    });
    await settle();
    expect(card().querySelector("[data-task-phase]")).toBeNull();
  });

  it("comando curto de agente não aparece; longo vira 'comando rodando'; os outros cartões ficam sem fase", async () => {
    const view = await mount(spec(TASKS()));
    const card = (id: string) => board().querySelector(`[data-task-id="${id}"]`) as HTMLElement;
    await act(async () => {
      view.stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", activity_id: "d1", kind: "command", text: "ls", command_started_at: Date.now() - 2_000 }));
    });
    await settle();
    expect(card("T02").querySelector("[data-task-phase]")).toBeNull();
    await act(async () => {
      view.stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", activity_id: "d2", kind: "command", text: "cargo build", command_started_at: Date.now() - 125_000 }));
    });
    await settle();
    expect(card("T02").querySelector("[data-task-phase]")?.textContent).toBe("comando rodando · 2 min");
    expect(card("T03").querySelector("[data-task-phase]")).toBeNull();
    // O comando do agente com o limite do servidor (5 min): perto dele, o mesmo aviso dos testes.
    await act(async () => {
      view.stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", activity_id: "d3", kind: "command", text: "cargo build", command_started_at: Date.now() - 250_000, command_limit_ms: 300_000 }));
    });
    await settle();
    const near = card("T02").querySelector("[data-task-phase]") as HTMLElement;
    expect(near.textContent).toBe("comando rodando · 4 min · perto do limite de tempo");
    expect(near.getAttribute("data-near-limit")).toBe("true");
  });
});
