/*
 * Janela Custos montada pelo shell (spec 093, T07; enxuta na 095/T05): em destaque o total, o
 * orçamento com a barra e os três maiores itens; em "Ver detalhes por tarefa e modelo" (recolhido)
 * categorias com TOTAL, agente, modelo, imagens da 092, barras por papel e por tarefa e o ritmo do
 * provedor; o orçamento editável e o `data-size`. Os testes do conteúdo recolhido abrem o
 * `summary` antes de procurar (C-11).
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent, UsageView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { fakeResizeObservers } from "../../desktop/test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { budgetProgress, costBars, pacingText, topCostItems } from "./costsModel";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  appStore.setState({ activities: {} });
});

const P1 = makeProject("p1", "Loja");
const totals = (cost: number | null, input = 1000, output = 500) => ({ calls: 2, input, output, cached: 0, reasoning: 0, cost_usd: cost, unpriced_calls: 0 });

const PROJECT: UsageView = {
  specs: [],
  total: { ...totals(0.13, 12_000, 4_000), images: 1, images_usd: 0.04, images_estimated: true, unpriced_calls: 2, web_search_calls: 3, web_search_usd: 0.03 },
  by_spec: [
    { id: "s1", ...totals(0.1) },
    { id: "chat", ...totals(0.03) },
  ],
  // Como no servidor: a imagem (US$ 0,04) fica dentro do custo do papel que a gerou; os papéis somam o total.
  by_agent: [
    { id: "developer", ...totals(0.08), images: 1, images_usd: 0.04, images_estimated: true },
    { id: "reviewer", ...totals(0.04) },
    { id: "architect", ...totals(0.01) },
  ],
  by_model: [{ id: "gpt-6-luna", ...totals(0.13) }],
  budget: null,
};

const SPEC: UsageView = {
  specs: [
    {
      spec: "s1",
      ...totals(0.1),
      agents: [],
      tasks: [
        { id: "T01", ...totals(0.06), active_secs: 600 },
        { id: "T02", ...totals(0.03), active_secs: 300 },
      ],
    },
  ],
  total: totals(0.1),
  budget: { max_usd: 5 },
};

async function openCosts(extra: Parameters<typeof renderShell>[0] = {}, project: UsageView = PROJECT) {
  const spec = makeSpec("s1", "Loja online", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS")]);
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec], running: true, run: { spec_id: "s1", status: "running" } }) },
    ...extra,
  });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, (_b, url) => ({ status: 200, body: url.includes("spec=") ? SPEC : project }));
  view.server.reply("PUT", /\/api\/projects\/p1\/specs\/s1\/budget/, (body) => ({ status: 200, body }));
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Custos" }));
  });
  await settle();
  return view;
}

function costs(): HTMLElement {
  return screen.getByRole("region", { name: "Custos" });
}

/** Abre um `<details>` da janela pelo `summary` (começa recolhido) e devolve o elemento. */
async function openDetails(selector: string): Promise<HTMLDetailsElement> {
  const details = await waitFor(() => {
    const el = costs().querySelector<HTMLDetailsElement>(selector);
    if (el === null) throw new Error(`sem ${selector}`);
    return el;
  });
  expect(details.open).toBe(false);
  const summary = details.querySelector("summary");
  if (summary === null) throw new Error("summary ausente");
  await act(async () => {
    fireEvent.click(summary);
  });
  expect(details.open).toBe(true);
  return details;
}

const openMore = (): Promise<HTMLDetailsElement> => openDetails("[data-costs-details]");

describe("Custos: em destaque (095/T05)", () => {
  it("total de 32 px com tabular-nums, orçamento 'de US$ X' com a barra e os três maiores itens; o resto recolhido", async () => {
    await openCosts();
    const win = costs();
    await waitFor(() => expect(within(win).getByTestId("usage-cost").textContent).toBe("US$ 0,13"));
    expect(within(win).getByTestId("images-hint").textContent).toBe("inclui 1 imagem (estimado)");
    const bar = await waitFor(() => {
      const el = win.querySelector<HTMLElement>("[data-budget-bar]");
      if (el === null) throw new Error("sem orçamento");
      return el;
    });
    expect(bar.textContent).toBe("Spec s1: US$ 0,10 de US$ 5,00");
    const progress = within(bar).getByRole("progressbar", { name: "Orçamento da spec s1" });
    expect(progress.getAttribute("aria-valuetext")).toBe("US$ 0,10 de US$ 5,00");
    expect(progress.getAttribute("aria-valuenow")).toBe("2");
    expect(bar.querySelector<HTMLElement>(".nd-costs-budget-fill")?.style.width).toBe("2%");
    const top = within(win).getByRole("region", { name: "Maiores custos" });
    // O desenvolvedor sem a imagem (0,08 menos 0,04); a imagem vira o próprio item.
    expect([...top.querySelectorAll("li")].map((li) => li.textContent)).toEqual(["DesenvolvedorUS$ 0,04", "RevisorUS$ 0,04", "Imagens (1)US$ 0,04 ~"]);
    // A soma dos itens exibidos nunca passa do total.
    const shown = [...top.querySelectorAll(".nd-costs-top-value")].map((el) => Number((el.textContent ?? "").replace(/[^\d,]/g, "").replace(",", ".")));
    expect(shown.reduce((a, b) => a + b, 0)).toBeLessThanOrEqual(0.13 + 1e-9);
    // "Ver detalhes por tarefa e modelo" e o orçamento começam recolhidos.
    const more = win.querySelector("[data-costs-details]") as HTMLDetailsElement;
    expect(more.open).toBe(false);
    expect(more.querySelector("summary")?.textContent).toBe("Ver detalhes por tarefa e modelo");
    expect((win.querySelector("[data-costs-budget]") as HTMLDetailsElement).open).toBe(false);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("CSS: total de 32 px em 600 com tabular-nums; raio cheio só nas barras de progresso", () => {
    const css = readFileSync(resolve(process.cwd(), "src/apps/costs/costs.css"), "utf8");
    const amount = css.slice(css.indexOf(".nd-costs-amount {"), css.indexOf("}", css.indexOf(".nd-costs-amount {")));
    expect(amount).toContain("font-size: var(--fs-numeral);");
    expect(amount).toContain("font-weight: var(--fw-semibold);");
    expect(amount).toContain("font-variant-numeric: tabular-nums;");
    const pills = [...css.matchAll(/([^{}]+)\{[^}]*border-radius: var\(--r-pill\)/g)].map((m) => (m[1] ?? "").trim().split("\n").at(-1));
    expect(pills).toEqual([".nd-costs-budget-track", ".nd-costs-budget-fill", ".nd-cost-bar-track", ".nd-cost-bar-fill"]);
  });

  it("sem orçamento: nenhuma barra; o resumo convida a definir um", async () => {
    const view = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [makeSpec("s1", "Loja online", "IN_PROGRESS", [makeTask("T01", "Base", "DONE")])] }) },
    });
    view.server.reply("GET", /\/api\/projects\/p1\/usage/, (_b, url) => ({ status: 200, body: url.includes("spec=") ? { ...SPEC, budget: null } : PROJECT }));
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Custos" }));
    });
    await settle();
    await waitFor(() => expect(costs().querySelector("[data-costs-budget] summary")?.textContent).toBe("Definir um orçamento para a spec"));
    expect(costs().querySelector("[data-budget-bar]")).toBeNull();
  });
});

describe("Custos: tudo do indicador de custo", () => {
  it("total com 'inclui N imagens (estimado)', categorias com TOTAL, agente, modelo, buscas, imagens e sem preço", async () => {
    await openCosts();
    const win = costs();
    await waitFor(() => expect(within(win).getByTestId("usage-cost").textContent).toContain("US$ 0,13"));
    expect(within(win).getByTestId("images-hint").textContent).toBe("inclui 1 imagem (estimado)");
    await openMore();
    expect(within(win).getByTestId("images-cost").textContent).toBe("Imagens geradas: 1 (US$ 0,04, estimado)");
    expect(within(win).getByTestId("web-search-cost").textContent).toBe("Buscas na web: 3 (US$ 0,03)");
    const note = within(win).getByRole("note");
    expect(note.textContent).toContain("2 chamadas sem preço");
    expect(within(note).getByRole("button", { name: "Configurar" })).toBeTruthy();
    const categories = within(win).getByRole("region", { name: "Por categoria" });
    expect(categories.textContent).toContain("Loja online");
    expect(categories.textContent).toContain("Conversa com o Arquiteto");
    expect(categories.querySelector("tfoot")?.textContent).toContain("TOTAL");
    expect(within(win).getByRole("region", { name: "Por agente" }).textContent).toContain("Desenvolvedor");
    expect(within(win).getByRole("region", { name: "Por modelo" }).textContent).toContain("gpt-6-luna");
    expect(within(win).getByTestId("web-search-cost").classList.contains("nd-num")).toBe(true);
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("barras por papel e por tarefa; ritmo do provedor sem esperas e com o aviso do limitador", async () => {
    await openCosts();
    const win = costs();
    await openMore();
    await waitFor(() => expect(within(win).getByRole("region", { name: "Por papel" })).toBeTruthy());
    const roles = within(win).getByRole("region", { name: "Por papel" });
    expect(roles.querySelector('[data-bar="developer"] .nd-cost-bar-fill')?.getAttribute("style")).toContain("width: 100%");
    expect(roles.querySelector('[data-bar="reviewer"] .nd-cost-bar-fill')?.getAttribute("style")).toContain("width: 50%");
    const tasks = await within(win).findByRole("region", { name: "Por tarefa · s1" });
    expect(tasks.textContent).toContain("T01 Base");
    expect(win.querySelector("[data-pacing]")?.textContent).toContain("Ritmo do provedor: sem esperas");
    await act(async () => {
      appStore.applyEvent({
        event_type: "agent_activity",
        project_id: "p1",
        payload: { agent: "developer", activity_id: "a1", kind: "text", text: "ajustando o ritmo ao limite do provedor: 12 s", code: "pace.waiting", params: { seconds: "12" } },
      } as unknown as ApiEvent);
    });
    expect(win.querySelector("[data-pacing]")?.getAttribute("data-pacing")).toBe("waiting");
    expect(win.querySelector("[data-pacing]")?.textContent).toContain("ajustando o ritmo ao limite do provedor: 12 s");
  });

  it("orçamento da spec: lê o limite atual, valida e salva", async () => {
    const { server } = await openCosts();
    const win = costs();
    const budget = await openDetails("[data-costs-budget]");
    expect(budget.querySelector("summary")?.textContent).toBe("Editar o orçamento da spec");
    const form = await within(win).findByRole("form", { name: "Orçamento da spec" });
    expect((within(form).getByLabelText("Limite em US$") as HTMLInputElement).value).toBe("5");
    fireEvent.change(within(form).getByLabelText("Limite de tokens"), { target: { value: "-3" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar limite" }));
    });
    expect(within(form).getByRole("status").textContent).toContain("Tokens: use um número inteiro");
    fireEvent.change(within(form).getByLabelText("Limite de tokens"), { target: { value: "50000" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Salvar limite" }));
    });
    await settle();
    const put = server.calls.find((c) => c.method === "PUT" && c.url.endsWith("/budget"));
    expect(put?.body).toEqual({ max_tokens: 50000, max_usd: 5 });
    expect(within(form).getByRole("status").textContent).toBe("Limite salvo.");
    // A barra acompanha o limite salvo: 1,5 mil de 50 mil tokens (3%) passa US$ 0,10 de 5,00 (2%).
    expect(win.querySelector("[data-budget-bar]")?.textContent).toBe("Spec s1: 1,5 mil tokens de 50,0 mil tokens");
    expect(within(win).getByRole("progressbar", { name: "Orçamento da spec s1" }).getAttribute("aria-valuenow")).toBe("3");
  });

  it("falha de carga: estado vazio, sem números", async () => {
    const spec = makeSpec("s1", "Loja online", "IN_PROGRESS", [makeTask("T01", "Base", "TODO")]);
    const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec] }) } });
    view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 500, body: { error: "falha" } }));
    await act(async () => {
      fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Custos" }));
    });
    await settle();
    expect(costs().textContent).toContain("Nenhum custo registrado neste projeto ainda.");
  });
});

describe("Custos: sem consumo, sem preço e ritmo (V-01)", () => {
  it("sem consumo: estado vazio próprio, nenhum número zerado em destaque", async () => {
    const zero = totals(0, 0, 0);
    await openCosts({}, { specs: [], total: { ...zero, calls: 0 }, by_spec: [], by_agent: [], by_model: [], budget: null });
    const win = costs();
    await waitFor(() => expect(win.textContent).toContain("Ainda não há consumo neste projeto."));
    expect(within(win).queryByTestId("usage-cost")).toBeNull();
    expect(win.textContent).not.toContain("sem preço");
    expect(win.textContent).not.toContain("0 chamadas");
    expect(win.querySelector("[data-empty-state] svg")).not.toBeNull();
  });

  it("com consumo e sem preço: 'Sem preço' em texto (sem traço), a nota e 'Configurar' na aba Preços", async () => {
    const unpriced = totals(null, 1200, 300);
    await openCosts({}, { specs: [], total: { ...unpriced, unpriced_calls: 2 }, by_spec: [{ id: "s1", ...unpriced }], by_agent: [{ id: "developer", ...unpriced }], by_model: [], budget: null });
    const win = costs();
    const amount = await within(win).findByTestId("usage-cost");
    expect(amount.hasAttribute("data-unpriced")).toBe(true);
    expect(amount.textContent).toBe("Sem preço");
    expect(amount.textContent).not.toContain("US$");
    const note = win.querySelector("[data-unpriced-note]") as HTMLElement;
    expect(note.textContent).toContain("Preço do modelo não cadastrado");
    expect(findForbiddenGlyphs(win)).toEqual([]);
    // Sem buscas na web, a linha delas não aparece.
    expect(within(win).queryByTestId("web-search-cost")).toBeNull();
    await act(async () => {
      fireEvent.click(within(note).getByRole("button", { name: "Configurar" }));
    });
    await settle();
    expect(screen.getByRole("region", { name: "Configurações" })).toBeTruthy();
  });

  it("ritmo do provedor: ícone e texto lado a lado no mesmo contêiner flexível", async () => {
    await openCosts();
    await openMore();
    const pacing = await waitFor(() => {
      const el = costs().querySelector<HTMLElement>("[data-pacing]");
      if (el === null) throw new Error("sem ritmo");
      return el;
    });
    expect(pacing.classList.contains("nd-costs-pacing")).toBe(true);
    expect([...pacing.children].map((c) => c.tagName.toLowerCase())).toEqual(["svg", "span"]);
    expect(pacing.children[1]?.textContent).toBe("Ritmo do provedor: sem esperas");
  });
});

describe("Custos: responsividade por data-size", () => {
  it("estreita: tabelas viram listas empilhadas", async () => {
    const observers = fakeResizeObservers();
    await openCosts({ createObserver: observers.factory });
    const win = costs();
    await openMore();
    await waitFor(() => expect(win.querySelector("[data-costs-layout]")?.getAttribute("data-costs-layout")).toBe("table"));
    expect(win.querySelector("table")).not.toBeNull();
    await act(async () => observers.resize(380));
    expect(win.getAttribute("data-size")).toBe("narrow");
    expect(win.querySelector("[data-costs-layout]")?.getAttribute("data-costs-layout")).toBe("stacked");
    expect(win.querySelector("table")).toBeNull();
    expect(win.querySelector("[data-stacked] [data-total]")?.textContent).toContain("TOTAL");
  });
});

describe("Custos: regras puras", () => {
  it("barras pelo custo; sem preço, pelos tokens", () => {
    expect(costBars([{ id: "a", ...totals(0.02) }, { id: "b", ...totals(0.01) }], (id) => id).map((b) => [b.pct, b.value])).toEqual([
      [100, "US$ 0,02"],
      [50, "US$ 0,01"],
    ]);
    expect(costBars([{ id: "a", ...totals(null, 300, 100) }, { id: "b", ...totals(null, 100, 100) }], (id) => id).map((b) => [b.pct, b.value])).toEqual([
      [100, "400 tokens"],
      [50, "200 tokens"],
    ]);
    expect(pacingText(null)).toBe("Ritmo do provedor: sem esperas");
  });

  it("três maiores: o custo das imagens sai do papel e vira 'Imagens (N)'; sem preço, pelos tokens", () => {
    const dev = { id: "developer", ...totals(0.36), images: 9, images_usd: 0.12 };
    const items = topCostItems({ ...totals(0.42), images: 9, images_usd: 0.12 }, [dev, { id: "reviewer", ...totals(0.04) }, { id: "architect", ...totals(0.02) }], (id) => id);
    expect(items).toEqual([
      { id: "developer", label: "developer", value: "US$ 0,24" },
      { id: "images", label: "Imagens (9)", value: "US$ 0,12" },
      { id: "reviewer", label: "reviewer", value: "US$ 0,04" },
    ]);
    expect(topCostItems(totals(null), [{ id: "a", ...totals(null, 300, 100) }], (id) => id)).toEqual([{ id: "a", label: "a", value: "400 tokens" }]);
  });

  it("orçamento: US$ quando há preço, senão tokens; acima do limite, a barra cheia e 'over'", () => {
    expect(budgetProgress(totals(0.5), { max_usd: 2 })).toEqual({ used: "US$ 0,50", limit: "de US$ 2,00", pct: 25, over: false });
    expect(budgetProgress(totals(null, 600, 400), { max_usd: 2, max_tokens: 500 })).toEqual({ used: "1,0 mil tokens", limit: "de 500 tokens", pct: 100, over: true });
    // Os dois limites: tokens acima e US$ abaixo; vale o de tokens (como o servidor, que para em qualquer um).
    expect(budgetProgress(totals(1.5, 40_000, 20_000), { max_tokens: 50_000, max_usd: 5 })).toEqual({ used: "60,0 mil tokens", limit: "de 50,0 mil tokens", pct: 100, over: true });
    // US$ abaixo e tokens mais perto do limite: a barra segue a maior razão.
    expect(budgetProgress(totals(1, 20_000, 20_000), { max_tokens: 50_000, max_usd: 5 })).toEqual({ used: "40,0 mil tokens", limit: "de 50,0 mil tokens", pct: 80, over: false });
    // Exatamente no limite: atingido (`>=`).
    expect(budgetProgress(totals(2), { max_usd: 2 })?.over).toBe(true);
    expect(budgetProgress(totals(0.5), {})).toBeNull();
    expect(budgetProgress(totals(0.5), null)).toBeNull();
  });
});
