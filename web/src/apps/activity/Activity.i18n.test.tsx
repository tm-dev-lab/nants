/*
 * Atividade nos dois idiomas (spec 102, T03): em en, nenhum texto visível ou acessível da janela
 * fica em português (o que os agentes escrevem e os avisos do servidor aparecem como vieram);
 * pt-BR igual a hoje; o subtítulo segue o idioma; a troca vale na hora.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent, ProjectState } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { manualScheduler } from "../../desktop/test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { setLanguage } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { activitySummary, liveActionOf } from "./activityModel";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  appStore.setState({ activities: {} });
  setLanguage("pt-BR", { persist: false });
});

const P1 = makeProject("p1", "Shop");

function running(): ProjectState {
  const spec = makeSpec("s1", "Online shop", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Cart", "IN_PROGRESS")]);
  return makeProjectState({ id: "p1", name: "Shop", specs: [spec], running: true, run: { spec_id: "s1", status: "running" } });
}

function activity(payload: Record<string, unknown>): ApiEvent {
  return { event_type: "agent_activity", project_id: "p1", payload: { activity_id: "a1", ...payload } } as unknown as ApiEvent;
}

const TRAIL = {
  items: [
    { seq: 3, ts: 1_699_999_990, agent: "developer", task_id: "T02", kind: "tool", text: "lendo src/cart.ts", code: "tool.read_file", params: { path: "src/cart.ts" } },
    { seq: 4, ts: 1_700_000_000, agent: "developer", task_id: "T02", kind: "tool", text: "write src/cart.ts" },
    { seq: 5, ts: 1_700_000_030, agent: "developer", task_id: "T02", kind: "command", text: "npm test" },
  ],
  total: 2,
  has_more: false,
};

const MANIFEST = /\/api\/projects\/p1\/file\?source=base&path=\.sdd%2Fspecs%2Fs1%2Fimages\.jsonl/;
const MANIFEST_TEXT = [
  JSON.stringify({ path: "web/img/cat.webp", task: "T02", at: "2026-10-19T12:00:00Z" }),
  JSON.stringify({ path: "web/img/dog.webp", task: "T02", at: "2026-10-19T12:01:00Z", reused: true }),
].join("\n");

async function mount(lang: "pt-BR" | "en", trail: typeof TRAIL = TRAIL, manifest: string | null = MANIFEST_TEXT) {
  setLanguage(lang, { persist: false });
  const scheduler = manualScheduler();
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: running() }, scheduler });
  view.server.reply("GET", /\/api\/projects\/p1\/specs\/s1\/tools/, () => ({ status: 200, body: trail }));
  view.server.reply("GET", MANIFEST, () =>
    manifest === null ? { status: 404, body: { error: "not found" } } : { status: 200, body: { kind: "text", size: manifest.length, content: manifest } },
  );
  await act(async () => {
    appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
  });
  await settle();
  return { ...view, scheduler };
}

function panel(lang: "pt-BR" | "en"): HTMLElement {
  return screen.getByRole("region", { name: lang === "en" ? "Activity" : "Atividade" });
}

describe("Atividade em en", () => {
  it("aba Ao vivo: linhas dos agentes, estados, etapa, últimas ações e subtítulo sem português", async () => {
    const { stream, scheduler } = await mount("en");
    const win = panel("en");
    expect(within(win).getByRole("tablist", { name: "Activity" })).toBeTruthy();
    expect(within(win).getByRole("tab", { name: "Live" }).getAttribute("aria-selected")).toBe("true");
    expect(within(win).getByRole("list", { name: "Agents" })).toBeTruthy();
    expect(win.querySelector('[data-agent-card="architect"]')?.textContent).toContain("available for questions");
    expect(win.querySelector('[data-agent-card="reviewer"]')?.textContent).toContain("waiting for its turn");
    expect(within(win).getByRole("region", { name: "Latest actions · T02" }).textContent).toContain("npm test");
    expect(within(win).getByRole("region", { name: "Latest actions · T02" }).textContent).toContain("reading src/cart.ts");
    expect(portugueseLeftovers(win)).toEqual([]);
    await act(async () => {
      // Spec 102 T06: o rótulo e a etapa chegam com `code`; o texto pt-BR é só a reserva.
      stream().emit(
        "agent_activity",
        activity({ agent: "developer", task_id: "T02", kind: "tool", text: "gerando imagem web/img/cat.webp", code: "tool.generate_image", params: { path: "web/img/cat.webp" } }),
      );
      stream().emit("agent_activity", activity({ agent: "reviewer", activity_id: "f1", kind: "start", stage: "final_verification", stage_code: "final_verification" }));
      stream().emit("agent_activity", activity({ agent: "architect", activity_id: "x1", kind: "start", stage: "Escrevendo relatório final", stage_code: "report" }));
      scheduler.runFrames();
    });
    const dev = win.querySelector('[data-agent-card="developer"]') as HTMLElement;
    expect(dev.querySelector("[data-live-state]")?.textContent).toBe("generating image");
    expect(dev.querySelector("[data-live-detail]")?.textContent).toBe("web/img/cat.webp");
    expect(win.querySelector('[data-agent-card="reviewer"] [data-live-stage]')?.textContent).toBe("Final verification in progress");
    expect(win.querySelector('[data-agent-card="architect"] [data-live-stage]')?.textContent).toBe("Writing the final report");
    expect(within(win).getByTestId("activity-summary").textContent).toMatch(/ active$/);
    expect(document.getElementById(windowSubtitleId("activity"))?.textContent).toBe("live");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("chamada longa ao modelo em en, sem português (spec 114 T04)", async () => {
    const { stream, scheduler } = await mount("en");
    await act(async () => {
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "model", model_started_at: Date.now() - 510_000 }));
      stream().emit("agent_activity", activity({ agent: "developer", task_id: "T02", kind: "heartbeat", received_bytes: 1_258_291 }));
      scheduler.runFrames();
    });
    const win = panel("en");
    const dev = win.querySelector('[data-agent-card="developer"]') as HTMLElement;
    expect(dev.querySelector("[data-live-state]")?.textContent).toBe("receiving the response");
    expect(dev.querySelector("[data-live-detail]")?.textContent).toBe("for 8 min · 1.2 MB received");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("aba Histórico: informativos e textos fixos sem português; o caminho aparece como veio", async () => {
    await mount("en");
    await act(async () => {
      appStore.appendMessage("p1", { role: "agent", agent: "reviewer", taskId: "T01", specId: "s1", attempt: 1, text: "Aprovado: all good" });
    });
    const win = panel("en");
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "History" }));
    });
    await settle();
    const history = within(win).getByRole("region", { name: "History" });
    expect(history.textContent).toContain("Updates for this spec.");
    expect(history.textContent).toContain("Reviewer approved T01");
    expect(history.textContent).toContain("attempt 1");
    expect(history.textContent).toContain("Image generated · T02");
    expect(history.textContent).toContain("web/img/dog.webp (reused)");
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("histórico vazio e sem ações recentes em en", async () => {
    await mount("en", { items: [], total: 0, has_more: false }, null);
    const win = panel("en");
    expect(within(win).getByText("No actions recorded yet.")).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "History" }));
    });
    await settle();
    expect(within(win).getByText("Nothing here yet. Reviewer approvals and generated images show up in this list.")).toBeTruthy();
    expect(portugueseLeftovers(win)).toEqual([]);
  });

  it("estados e subtítulos pelo modelo no idioma ativo", () => {
    setLanguage("en", { persist: false });
    const base = { agent: "developer", reasoning: "", text: "", tool: "" };
    expect(liveActionOf({ ...base, tool: "read_file src/a.ts" }).state).toBe("working");
    expect(liveActionOf({ ...base, text: "Hello" }).state).toBe("writing");
    expect(liveActionOf({ ...base, reasoning: "hm" }).state).toBe("thinking");
    expect(liveActionOf({ ...base, command: "ls" }).state).toBe("running");
    expect(liveActionOf({ ...base, agent: "tests", command: "npm test" }).state).toBe("running the tests");
    // Avisos do servidor pelo `code` (spec 102 T06); a nova tentativa ainda pela marca, com a contagem traduzida.
    const pacing = liveActionOf({
      ...base,
      text: "ajustando o ritmo ao limite do provedor: 12 s",
      textCode: { code: "pace.waiting", params: { seconds: "12" } },
    });
    expect(pacing).toMatchObject({ state: "adjusting the pace", detail: "adjusting the pace to the provider limit: 12 s" });
    // Spec 119: o motivo antigo em pt-BR é lido de volta pelo dicionário; fora do vocabulário, só a contagem.
    expect(liveActionOf({ ...base, text: "resposta do provedor falhou (incompleta: x); repetindo (1/2)" })).toMatchObject({
      state: "retrying",
      detail: "the provider response failed (incomplete: x); retrying (1/2)",
    });
    expect(liveActionOf({ ...base, text: "resposta do provedor falhou (outra); repetindo (1/2)" })).toMatchObject({
      state: "retrying",
      detail: "attempt 1 of 2",
    });
    expect(liveActionOf({ ...base, tool: "lendo a.ts", toolCode: { code: "tool.read_file", params: { path: "a.ts" } } }).detail).toBe("reading a.ts");
    const tests = liveActionOf({ ...base, agent: "tests", command: "npm test", stage: "Testes · tentativa 2", stageCode: { code: "tests", params: { attempt: 2 } } });
    expect(tests.stage).toBe("Tests · attempt 2");
    const check = { code: "check_base", params: { attempt: 1, check: "build" } };
    expect(liveActionOf({ ...base, stage: "Checagem build na base · tentativa 1", stageCode: check }).stage).toBe("Check build on the base · attempt 1");
    // Registro antigo, sem `code`: a etapa aparece como veio.
    expect(liveActionOf({ ...base, stage: "Escrevendo relatório final" }).stage).toBe("Escrevendo relatório final");
    const space = { running: false, architectTurnActive: false, paused: false, stalled: false };
    expect(activitySummary({ space } as never)).toEqual({ subtitle: "agents idle" });
    expect(activitySummary({ space: { ...space, architectTurnActive: true } } as never)).toEqual({ subtitle: "Architect working" });
    expect(activitySummary({ space: { ...space, paused: true } } as never)).toEqual({ subtitle: "run paused" });
    expect(activitySummary({ space: { ...space, stalled: true } } as never)).toEqual({ subtitle: "run stopped" });
  });
});

describe("Atividade em pt-BR e troca ao vivo", () => {
  it("pt-BR igual a hoje e a troca vale na hora", async () => {
    await mount("pt-BR");
    let win = panel("pt-BR");
    expect(within(win).getByRole("tab", { name: "Ao vivo" })).toBeTruthy();
    expect(win.querySelector('[data-agent-card="architect"]')?.textContent).toContain("disponível para perguntas");
    expect(within(win).getByRole("region", { name: "Últimas ações · T02" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("activity"))?.textContent).toBe("ao vivo");
    expect(portugueseLeftovers(win).length).toBeGreaterThan(0);
    await act(async () => {
      appStore.appendMessage("p1", { role: "agent", agent: "reviewer", taskId: "T01", specId: "s1", attempt: 1, text: "Aprovado: tudo certo" });
    });
    await act(async () => {
      fireEvent.click(within(win).getByRole("tab", { name: "Histórico" }));
    });
    await settle();
    expect(within(win).getByRole("region", { name: "Histórico" }).textContent).toContain("Revisor aprovou T01");

    await act(async () => {
      setLanguage("en", { persist: false });
    });
    win = panel("en");
    const history = within(win).getByRole("region", { name: "History" });
    expect(history.textContent).toContain("Reviewer approved T01");
    expect(history.textContent).toContain("Image generated · T02");
    expect(within(win).getByRole("tab", { name: "Live" })).toBeTruthy();
    expect(document.getElementById(windowSubtitleId("activity"))?.textContent).toBe("live");
    expect(portugueseLeftovers(win)).toEqual([]);
  });
});
