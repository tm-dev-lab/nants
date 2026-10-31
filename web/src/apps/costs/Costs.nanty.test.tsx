/*
 * O gasto do Nanty na janela Custos (spec 138 T10, critério 4, C-17): uma seção à parte do projeto,
 * com US$ quando há preço, os tokens, as chamadas sem preço, as de voz, o orçamento do dia e os dias
 * anteriores; também com o projeto sem consumo. Relida pelo `nanty_changed`.
 */
import { act, cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ApiEvent, UsageView } from "../../api/types";
import { activateApp, NANTY_EMPTY_USAGE, renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
});

const TOTAL = { calls: 4, input: 120_000, output: 8_000, cached: 0, reasoning: 0, cost_usd: 0.42, unpriced_calls: 0 };
const PROJECT: UsageView = { specs: [], total: TOTAL, by_agent: [{ id: "developer", ...TOTAL }], by_spec: [], by_model: [], budget: null };
const EMPTY_PROJECT: UsageView = { ...PROJECT, total: { ...TOTAL, calls: 0, input: 0, output: 0, cost_usd: null }, by_agent: [] };

const SPENT = {
  ...NANTY_EMPTY_USAGE,
  today: { day: "2026-10-30", usd: 0.06, input_tokens: 900, output_tokens: 80, unpriced_turns: 1, voice_calls: 2 },
  history: [{ day: "2026-10-29", usd: 0, input_tokens: 1200, output_tokens: 300, unpriced_turns: 3, voice_calls: 0 }],
};

async function openCosts(project: UsageView, usage: unknown, dock = "Custos") {
  const view = await renderShell({
    url: "/?space=p1",
    projects: [makeProject("p1", "Loja")],
    states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
    nanty: { usage },
  });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: project }));
  await act(async () => {
    activateApp(dock);
  });
  await settle();
  return { view, win: screen.getByRole("region", { name: dock }) };
}

describe("Custos: o gasto do Nanty (C-17)", () => {
  it("uma seção à parte do projeto: US$, tokens, sem preço, voz, orçamento e dias anteriores", async () => {
    const { win } = await openCosts(PROJECT, SPENT);
    const section = win.querySelector<HTMLElement>("[data-costs-nanty]") as HTMLElement;
    expect(section.querySelector("h3")?.textContent).toBe("Nanty");
    expect(section.textContent).toContain("O gasto do Nanty fica fora do custo dos projetos.");
    expect(section.querySelector("[data-nanty-today]")?.textContent).toBe("Hoje US$ 0,06 · 980 tokens");
    expect(section.querySelector("[data-nanty-budget]")?.textContent).toBe("Orçamento do dia: US$ 0,50");
    expect(section.querySelector("[data-nanty-unpriced]")?.textContent).toBe("1 chamada sem preço: conta só os tokens.");
    expect(section.querySelector("[data-nanty-voice]")?.textContent).toBe("2 chamadas de voz (fala ou transcrição), já somadas acima.");
    expect(section.querySelector("[data-nanty-history]")?.textContent).toContain("29/10/2026");
    expect(section.querySelector("[data-nanty-history]")?.textContent).toContain("1.500 tokens");
    // O total do projeto não leva o Nanty.
    expect(win.querySelector('[data-testid="usage-cost"]')?.textContent).toBe("US$ 0,42");
  });

  it("projeto sem consumo: o estado vazio e o gasto do Nanty abaixo", async () => {
    const { win } = await openCosts(EMPTY_PROJECT, SPENT);
    expect(win.textContent).toContain("Ainda não há consumo neste projeto.");
    expect(win.querySelector("[data-costs-nanty]")).not.toBeNull();
  });

  it("sem gasto do Nanty, nenhuma seção", async () => {
    const { win } = await openCosts(PROJECT, NANTY_EMPTY_USAGE);
    expect(win.querySelector("[data-costs-nanty]")).toBeNull();
  });

  it("o nanty_changed relê o gasto; em tokens quando o modelo não tem preço", async () => {
    const { view, win } = await openCosts(PROJECT, NANTY_EMPTY_USAGE);
    view.server.reply("GET", /^\/api\/nanty\/usage$/, () => ({
      status: 200,
      body: { ...NANTY_EMPTY_USAGE, budget: { ...NANTY_EMPTY_USAGE.budget, mode: "tokens" }, today: { ...SPENT.today, usd: 0, voice_calls: 1 } },
    }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "feed" } } as unknown as ApiEvent);
    });
    await settle();
    const section = win.querySelector<HTMLElement>("[data-costs-nanty]") as HTMLElement;
    expect(section.querySelector("[data-nanty-today]")?.textContent).toBe("Hoje 980 tokens");
    expect(section.querySelector("[data-nanty-budget]")?.textContent).toBe("Orçamento do dia em tokens: 200.000");
    expect(section.querySelector("[data-nanty-voice]")?.textContent).toBe("1 chamada de voz (fala ou transcrição), já somada acima.");
  });

  it("em en, sem português", async () => {
    setLanguage("en", { persist: false });
    const { win } = await openCosts(PROJECT, SPENT, "Costs");
    const section = win.querySelector<HTMLElement>("[data-costs-nanty]") as HTMLElement;
    expect(section.querySelector("[data-nanty-today]")?.textContent).toBe("Today US$ 0.06 · 980 tokens");
    expect(portugueseLeftovers(section)).toEqual([]);
  });
});
