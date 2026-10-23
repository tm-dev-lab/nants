/*
 * Custos com o plano ChatGPT (spec 116 T03): a seção "Plano ChatGPT" com os tokens e as chamadas,
 * sem US$, e o link "Gerenciar uso". Só pelo plano, nada de total em dólar.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UsageView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { makeProject, makeProjectState } from "../../test-utils";
import { PLAN_USAGE_URL } from "../settings/chatgptPlan";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
});

const zero = { calls: 0, input: 0, output: 0, cached: 0, reasoning: 0, cost_usd: null, unpriced_calls: 0 };
const PLAN = { calls: 7, input: 120_000, output: 30_000, cached: 0, reasoning: 0 };

async function openCosts(project: UsageView) {
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: project }));
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Custos" }));
  });
  await settle();
  return screen.getByRole("region", { name: "Custos" });
}

describe("Custos: Plano ChatGPT", () => {
  it("só pelo plano: a seção com os tokens, sem US$ e sem o total em dólar", async () => {
    const win = await openCosts({ specs: [], total: { ...zero, chatgpt_plan: PLAN }, by_agent: [], by_spec: [], by_model: [], budget: null });
    const plan = within(win).getByRole("region", { name: "Plano ChatGPT" });
    expect(plan.querySelector("[data-plan-tokens]")?.textContent).toBe("120,0 mil entrada, 30,0 mil saída · 7 chamadas");
    expect(plan.textContent).toContain("Sai do limite de uso do plano, sem US$.");
    expect(plan.textContent).not.toMatch(/US\$ \d/);
    const link = within(plan).getByRole("link", { name: "Gerenciar uso" });
    expect(link.getAttribute("href")).toBe(PLAN_USAGE_URL);
    expect(link.getAttribute("rel")).toBe("noopener noreferrer");
    expect(win.querySelector("[data-testid='usage-cost']")).toBeNull();
    expect(win.querySelector("[data-costs-details]")).toBeNull();
    expect(win.textContent).not.toContain("Nenhum consumo");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("com chave e plano: o total em dólar e a seção do plano", async () => {
    const api = { calls: 2, input: 1000, output: 500, cached: 0, reasoning: 0, cost_usd: 0.13, unpriced_calls: 0 };
    const win = await openCosts({ specs: [], total: { ...api, chatgpt_plan: PLAN }, by_agent: [{ id: "architect", ...api }], by_spec: [], by_model: [], budget: null });
    expect(win.querySelector("[data-testid='usage-cost']")?.textContent).toBe("US$ 0,13");
    expect(within(win).getByRole("region", { name: "Plano ChatGPT" })).toBeTruthy();
  });

  it("sem uso pelo plano, sem a seção", async () => {
    const api = { calls: 2, input: 1000, output: 500, cached: 0, reasoning: 0, cost_usd: 0.13, unpriced_calls: 0 };
    const win = await openCosts({ specs: [], total: api, by_agent: [], by_spec: [], by_model: [], budget: null });
    expect(win.querySelector("[data-costs-plan]")).toBeNull();
  });

  it("em en", async () => {
    setLanguage("en", { persist: false });
    const win = await openCostsEn();
    const plan = within(win).getByRole("region", { name: "ChatGPT plan" });
    expect(plan.textContent).toContain("Counts against the plan's usage limit, with no US$ cost.");
    expect(within(plan).getByRole("link", { name: "Manage usage" })).toBeTruthy();
  });
});

async function openCostsEn() {
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({
    status: 200,
    body: { specs: [], total: { ...zero, chatgpt_plan: PLAN }, by_agent: [], by_spec: [], by_model: [], budget: null },
  }));
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Costs" }));
  });
  await settle();
  return screen.getByRole("region", { name: "Costs" });
}
