/*
 * Custos com a escrita de cache (spec 132 T02/T10): nos detalhes, a linha com os tokens lidos do
 * cache e os gravados nele, só quando o provedor informa a escrita. Nos dois idiomas.
 */
import { act, cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UsageView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { makeProject, makeProjectState } from "../../test-utils";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.unstubAllGlobals();
});

const TOTAL = { calls: 4, input: 120_000, output: 8_000, cached: 90_000, reasoning: 0, cache_write: 12_000, cost_usd: 0.42, unpriced_calls: 0 };

async function openCosts(project: UsageView, dock = "Custos") {
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
  view.server.reply("GET", /\/api\/projects\/p1\/usage/, () => ({ status: 200, body: project }));
  await act(async () => {
    activateApp(dock);
  });
  await settle();
  return screen.getByRole("region", { name: dock });
}

const usage = (total: UsageView["total"]): UsageView => ({ specs: [], total, by_agent: [{ id: "developer", ...total }], by_spec: [], by_model: [], budget: null });

describe("Custos: escrita de cache", () => {
  it("a linha do cache nos detalhes, com lidos e gravados", async () => {
    const win = await openCosts(usage(TOTAL));
    const line = win.querySelector<HTMLElement>("[data-costs-details] [data-costs-cache]");
    expect(line?.textContent).toBe("90,0 mil tokens lidos do cache e 12,0 mil gravados nele");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("sem escrita de cache, sem a linha", async () => {
    const { cache_write: _drop, ...rest } = TOTAL;
    const win = await openCosts(usage(rest));
    expect(win.querySelector("[data-costs-cache]")).toBeNull();
  });

  it("em en", async () => {
    setLanguage("en", { persist: false });
    const win = await openCosts(usage(TOTAL), "Costs");
    const line = win.querySelector<HTMLElement>("[data-costs-cache]");
    expect(line?.textContent).toBe("90.0K tokens read from the cache and 12.0K written to it");
    expect(portugueseLeftovers(line ?? win)).toEqual([]);
  });
});
