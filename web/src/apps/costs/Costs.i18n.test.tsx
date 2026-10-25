/*
 * Custos nos dois idiomas (spec 102, T03): em en, nenhum texto visível ou acessível da janela fica
 * em português; números, US$ e tokens pelos formatadores da 101 no idioma ativo; pt-BR igual a
 * hoje; o subtítulo segue o idioma; a troca vale na hora (inclusive o aviso do formulário).
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UsageView } from "../../api/types";
import { imagesCostHint, imagesCostLine, webSearchCostLine } from "../../copy";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { formatTokens, formatUsd } from "../../usageFormat";
import { budgetProgress, contextEffectText, contextRows, costsSummary, pacingText } from "./costsModel";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  appStore.setState({ activities: {} });
  setLanguage("pt-BR", { persist: false });
});

const P1 = makeProject("p1", "Shop");
const totals = (cost: number | null, input = 1000, output = 500) => ({ calls: 2, input, output, cached: 0, reasoning: 0, cost_usd: cost, unpriced_calls: 0 });

// Resumo do contexto (spec 122): sem a estimativa do servidor (antigo), tokens evitados = bytes / 4
// (340.000 -> 85,0 mil; 40.000 -> 10,0 mil).
const ctx = (compactions: number, summaries: number, saved_bytes: number) => ({ calls: 2, input: 1000, avg_input: 500, max_input: 600, max_context_bytes: 2400, compactions, summaries, saved_bytes });
// Com os campos da 123/T03: a estimativa do servidor manda (12.620.914 tokens, como no Test 3).
const ctxFull = { calls: 1456, input: 33_227_375, avg_input: 22_820, max_input: 52_130, max_context_bytes: 151_678, cached: 8_780_800, cache_pct: 26, compactions: 88, spills: 21, dedups: 2, summaries: 0, saved_bytes: 2_761_500, reinjection_avoided_tokens: 12_620_914 };

const PROJECT: UsageView = {
  specs: [],
  context: ctx(12, 1, 340_000),
  total: { ...totals(1234.5, 12_000, 4_000), cost_estimated: true, images: 2, images_usd: 0.04, images_estimated: true, unpriced_calls: 2, web_search_calls: 3, web_search_usd: 0.03 },
  by_spec: [
    { id: "s1", ...totals(1000) },
    { id: "chat", ...totals(234.5) },
  ],
  by_agent: [
    { id: "developer", ...totals(1000), images: 2, images_usd: 0.04, images_estimated: true, unpriced_calls: 1 },
    { id: "reviewer", ...totals(200) },
    { id: "architect", ...totals(34.5) },
  ],
  by_model: [{ id: "gpt-6-luna", ...totals(1234.5) }],
  budget: null,
};

const SPEC: UsageView = {
  specs: [{ spec: "s1", ...totals(0.1), agents: [], tasks: [{ id: "T01", ...totals(0.06), active_secs: 600 }] }],
  total: totals(0.1),
  budget: { max_usd: 5 },
  context: ctx(3, 0, 40_000),
};

async function openCosts(lang: "pt-BR" | "en", project: UsageView = PROJECT) {
  setLanguage(lang, { persist: false });
  const spec = makeSpec("s1", "Online shop", "IN_PROGRESS", [makeTask("T01", "Base", "DONE")]);
  const view = await renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Shop", specs: [spec], running: true, run: { spec_id: "s1", status: "running" } }) },
  });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, (_b, url) => ({ status: 200, body: url.includes("spec=") ? SPEC : project }));
  view.server.reply("PUT", /\/api\/projects\/p1\/specs\/s1\/budget/, (body) => ({ status: 200, body }));
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: lang === "en" ? "Costs" : "Custos" }));
  });
  await settle();
  return view;
}

function costs(lang: "pt-BR" | "en"): HTMLElement {
  return screen.getByRole("region", { name: lang === "en" ? "Costs" : "Custos" });
}

async function openAll(win: HTMLElement): Promise<void> {
  await waitFor(() => expect(win.querySelector("[data-costs-budget]")).not.toBeNull());
  await act(async () => {
    for (const details of win.querySelectorAll("details")) details.open = true;
  });
}

describe("Custos em en", () => {
  it("destaque, detalhes, tabelas e orçamento sem português; números e US$ em en", async () => {
    await openCosts("en");
    const win = costs("en");
    await waitFor(() => expect(within(win).getByTestId("usage-cost").textContent).toBe("US$ 1,234.50 (estimated)"));
    expect(within(win).getByRole("region", { name: "Project cost" })).toBeTruthy();
    expect(within(win).getByTestId("images-hint").textContent).toBe("includes 2 images (estimated)");
    await waitFor(() => expect(win.querySelector("[data-budget-bar]")?.textContent).toBe("Spec s1: US$ 0.10 of US$ 5.00"));
    expect([...within(win).getByRole("region", { name: "Context" }).querySelectorAll("[data-costs-context-line]")].map((p) => p.textContent)).toEqual([
      "Context management: 12 compactions, 1 summary, about 85.0K tokens avoided (estimate)",
      "In spec s1: 3 compactions, 0 summaries, about 10.0K tokens avoided",
    ]);
    expect(within(win).getByRole("progressbar", { name: "Spec s1 budget" })).toBeTruthy();
    expect([...within(win).getByRole("region", { name: "Top costs" }).querySelectorAll("li")].map((li) => li.textContent)).toEqual([
      "DeveloperUS$ 999.96",
      "ReviewerUS$ 200.00",
      "ArchitectUS$ 34.50",
    ]);
    expect(win.querySelector("[data-costs-details] summary")?.textContent).toBe("See details by task and model");
    expect(win.querySelector("[data-costs-budget] summary")?.textContent).toBe("Edit the spec budget");
    expect(win.querySelector("[data-costs-context-details] summary")?.textContent).toBe("See context details");
    await openAll(win);
    const scopes = [...win.querySelectorAll("[data-context-scope]")];
    expect(scopes.map((el) => el.querySelector("h3")?.textContent)).toEqual(["In the project", "In spec s1"]);
    expect([...(scopes[0]?.querySelectorAll("[data-context-row]") ?? [])].map((r) => `${r.querySelector("dt")?.textContent}: ${r.querySelector("dd")?.textContent}`)).toEqual([
      "Calls: 2",
      "Average input per call: 500 tokens",
      "Largest context: 600 tokens",
      "Compactions: 12",
      "History summaries: 1",
      "Tokens avoided (estimate): 85.0K tokens",
    ]);
    expect(win.querySelector("[data-costs-context-note]")?.textContent).toBe(
      "Tokens avoided are an estimate: what each compaction, saved output, avoided read or summary removed from the history, times the calls the same agent session made afterwards. It is not an exact value.",
    );
    expect(win.querySelector("[data-costs-tokens]")?.textContent).toBe("12.0K input, 4.0K output · 2 calls");
    expect(within(win).getByTestId("images-cost").textContent).toBe("Generated images: 2 (US$ 0.04, estimated)");
    expect(within(win).getByTestId("web-search-cost").textContent).toBe("Web searches: 3 (US$ 0.03)");
    expect(win.querySelector("[data-pacing]")?.textContent).toBe("Provider pace: no waits");
    const note = win.querySelector("[data-unpriced-calls]") as HTMLElement;
    expect(note.textContent).toContain("2 calls without a configured price are left out of the cost.");
    expect(within(note).getByRole("button", { name: "Set up" })).toBeTruthy();
    const categories = within(win).getByRole("region", { name: "By category" });
    expect(categories.textContent).toContain("Conversation with the Architect");
    expect([...categories.querySelectorAll("thead th")].map((th) => th.textContent)).toEqual(["Category", "Input", "Output", "Cost"]);
    expect(within(win).getByRole("region", { name: "By agent" }).textContent).toContain("1 unpriced");
    expect(within(win).getByRole("region", { name: "By model" }).textContent).toContain("Each call is priced at its own model's price");
    await waitFor(() => expect(within(win).getByRole("region", { name: "By task · s1" })).toBeTruthy());
    expect(within(win).getByRole("region", { name: "By role" })).toBeTruthy();
    const form = within(win).getByRole("form", { name: "Spec budget" });
    expect(within(form).getByLabelText("Token limit")).toBeTruthy();
    expect(within(form).getByLabelText("Limit in US$")).toBeTruthy();
    expect(form.textContent).toContain("Current limit: no token limit · US$ 5.00");
    expect(document.getElementById(windowSubtitleId("costs"))?.textContent).toBe("live · this project");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("formulário do orçamento: validação e confirmação em en; o aviso troca de idioma ao vivo", async () => {
    await openCosts("en");
    const win = costs("en");
    await openAll(win);
    const form = await within(win).findByRole("form", { name: "Spec budget" });
    fireEvent.change(within(form).getByLabelText("Token limit"), { target: { value: "-3" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save limit" }));
    });
    expect(within(form).getByRole("status").textContent).toBe("Tokens: use a whole number greater than or equal to zero.");
    fireEvent.change(within(form).getByLabelText("Token limit"), { target: { value: "50000" } });
    await act(async () => {
      fireEvent.click(within(form).getByRole("button", { name: "Save limit" }));
    });
    await settle();
    expect(within(form).getByRole("status").textContent).toBe("Limit saved.");
    expect(win.querySelector("[data-budget-bar]")?.textContent).toBe("Spec s1: 1.5K tokens of 50.0K tokens");
    expect(portugueseLeftovers(win)).toEqual([]);
    await act(async () => {
      setLanguage("pt-BR", { persist: false });
    });
    expect(within(costs("pt-BR")).getByRole("status").textContent).toBe("Limite salvo.");
    expect(costs("pt-BR").querySelector("[data-budget-bar]")?.textContent).toBe("Spec s1: 1,5 mil tokens de 50,0 mil tokens");
  });

  it("sem consumo e sem preço em en", async () => {
    const zero = totals(0, 0, 0);
    await openCosts("en", { specs: [], total: { ...zero, calls: 0 }, by_spec: [], by_agent: [], by_model: [], budget: null });
    await waitFor(() => expect(costs("en").textContent).toContain("No usage in this project yet."));
    expect(portugueseLeftovers(costs("en"))).toEqual([]);
    cleanup();
    await openCosts("en", { ...PROJECT, total: { ...PROJECT.total, cost_usd: null } });
    const win = costs("en");
    await waitFor(() => expect(within(win).getByTestId("usage-cost").textContent).toBe("No price"));
    expect(win.querySelector("[data-unpriced-note]")?.textContent).toContain("Model price not set");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("funções do modelo e de copy no idioma ativo", () => {
    setLanguage("en", { persist: false });
    expect(formatTokens(1_234)).toBe("1.2K");
    expect(formatTokens(2_500_000)).toBe("2.5M");
    expect(formatUsd(0.004)).toBe("US$ 0.0040");
    expect(webSearchCostLine(1, "US$ 0.01")).toBe("Web searches: 1 (US$ 0.01)");
    expect(imagesCostHint(1, false)).toBe("includes 1 image");
    expect(imagesCostLine(2, "US$ 0.08", false)).toBe("Generated images: 2 (US$ 0.08)");
    expect(pacingText("ajustando o ritmo ao limite do provedor: 12 s")).toBe("Provider pace: ajustando o ritmo ao limite do provedor: 12 s");
    expect(budgetProgress({ ...totals(0.5), input: 0, output: 0 }, { max_usd: 1 })?.limit).toBe("of US$ 1.00");
    expect(costsSummary({ space: { running: false } } as never)).toEqual({ subtitle: "this project" });
    setLanguage("pt-BR", { persist: false });
    expect(formatTokens(1_234)).toBe("1,2 mil");
    expect(formatTokens(2_500_000)).toBe("2,5M");
    expect(formatUsd(0.004)).toBe("US$ 0,0040");
    expect(imagesCostHint(2, true)).toBe("inclui 2 imagens (estimado)");
    expect(costsSummary({ space: { running: true } } as never)).toEqual({ subtitle: "ao vivo · este projeto" });
  });
});

describe("Custos em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora", async () => {
    await openCosts("pt-BR");
    let win = costs("pt-BR");
    await waitFor(() => expect(within(win).getByTestId("usage-cost").textContent).toBe("US$ 1.234,50 (estimado)"));
    expect(within(win).getByTestId("images-hint").textContent).toBe("inclui 2 imagens (estimado)");
    await waitFor(() => expect(win.querySelector("[data-budget-bar]")?.textContent).toBe("Spec s1: US$ 0,10 de US$ 5,00"));
    expect([...within(win).getByRole("region", { name: "Contexto" }).querySelectorAll("[data-costs-context-line]")].map((p) => p.textContent)).toEqual([
      "Gestão de contexto: 12 compactações, 1 resumo, cerca de 85,0 mil tokens evitados (estimativa)",
      "Na spec s1: 3 compactações, 0 resumos, cerca de 10,0 mil tokens evitados",
    ]);
    expect(win.querySelector("[data-costs-details] summary")?.textContent).toBe("Ver detalhes por tarefa e modelo");
    await openAll(win);
    expect(win.querySelector("[data-costs-tokens]")?.textContent).toBe("12,0 mil entrada, 4,0 mil saída · 2 chamadas");
    expect(within(win).getByTestId("images-cost").textContent).toBe("Imagens geradas: 2 (US$ 0,04, estimado)");
    expect(within(win).getByTestId("web-search-cost").textContent).toBe("Buscas na web: 3 (US$ 0,03)");
    expect(document.getElementById(windowSubtitleId("costs"))?.textContent).toBe("ao vivo · este projeto");
    expect(portugueseLeftovers(win).length).toBeGreaterThan(0);

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    win = costs("en");
    expect(within(win).getByTestId("usage-cost").textContent).toBe("US$ 1,234.50 (estimated)");
    expect(win.querySelector("[data-costs-tokens]")?.textContent).toBe("12.0K input, 4.0K output · 2 calls");
    expect(win.querySelector("[data-budget-bar]")?.textContent).toBe("Spec s1: US$ 0.10 of US$ 5.00");
    expect(win.querySelector("[data-costs-context] p")?.textContent).toBe("Context management: 12 compactions, 1 summary, about 85.0K tokens avoided (estimate)");
    expect(document.getElementById(windowSubtitleId("costs"))?.textContent).toBe("live · this project");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("efeito da gestão de contexto: some sem efeito ou sem o resumo (servidor antigo); singular e plural", () => {
    setLanguage("pt-BR", { persist: false });
    expect(contextEffectText(undefined)).toBeNull();
    expect(contextEffectText(ctx(0, 0, 0))).toBeNull();
    expect(contextEffectText(ctx(1, 2, 2_000))).toBe("Gestão de contexto: 1 compactação, 2 resumos, cerca de 500 tokens evitados (estimativa)");
    expect(contextEffectText(ctx(0, 0, 4_000), "s9")).toBe("Na spec s9: 0 compactações, 0 resumos, cerca de 1,0 mil tokens evitados");
    setLanguage("en", { persist: false });
    expect(contextEffectText(ctx(1, 1, 2_000))).toBe("Context management: 1 compaction, 1 summary, about 500 tokens avoided (estimate)");
    expect(contextEffectText(ctx(2, 0, 0), "s9")).toBe("In spec s9: 2 compactions, 0 summaries, about 0 tokens avoided");
  });

  it("área Contexto (123/T03): a estimativa do servidor no resumo e nos detalhes, nos dois idiomas", () => {
    setLanguage("pt-BR", { persist: false });
    expect(contextEffectText(ctxFull)).toBe("Gestão de contexto: 88 compactações, 0 resumos, cerca de 12,6M tokens evitados (estimativa)");
    expect(contextRows(ctxFull).map((r) => `${r.label}: ${r.value}`)).toEqual([
      "Chamadas: 1.456",
      "Entrada média por chamada: 22,8 mil tokens",
      "Maior contexto: 52,1 mil tokens",
      "Entrada lida do cache: 26%",
      "Compactações: 88",
      "Saídas grandes guardadas em arquivo: 21",
      "Leituras repetidas evitadas: 2",
      "Resumos do histórico: 0",
      "Tokens evitados (estimativa): 12,6M tokens",
    ]);
    setLanguage("en", { persist: false });
    expect(contextEffectText(ctxFull, "s9")).toBe("In spec s9: 88 compactions, 0 summaries, about 12.6M tokens avoided");
    expect(contextRows(ctxFull).map((r) => `${r.label}: ${r.value}`)).toEqual([
      "Calls: 1,456",
      "Average input per call: 22.8K tokens",
      "Largest context: 52.1K tokens",
      "Input read from cache: 26%",
      "Compactions: 88",
      "Large outputs saved to a file: 21",
      "Repeated reads avoided: 2",
      "History summaries: 0",
      "Tokens avoided (estimate): 12.6M tokens",
    ]);
  });
});
