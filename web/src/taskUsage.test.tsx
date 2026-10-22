/* Tempo e custo por tarefa (linha do cartão do Quadro) e recarga ao vivo com debounce (086/T06). */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { parseApiEvent } from "./api/events";
import type { ApiEvent, TaskUsage, UsageView } from "./api/types";
import { LIVE_RELOAD_MS, appStore, createAppStore } from "./store";
import { installFetch, resetStore } from "./test-utils";
import { formatActive, taskUsageLine } from "./usageFormat";
import { taskUsageOf, useUsageView } from "./apps/shared/useUsage";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const base = { calls: 2, input: 10, output: 5, cached: 0, reasoning: 0, unpriced_calls: 0 };

function usage(id: string, active: number, cost: number | null, estimated = false): TaskUsage {
  return { id, ...base, cost_usd: cost, cost_estimated: estimated, active_secs: active, first_at: "2026-10-18T10:00:00Z", last_at: "2026-10-18T10:12:00Z" };
}

describe("linha de tempo e custo", () => {
  it("formata o tempo ativo", () => {
    expect(formatActive(0)).toBe("< 1 min");
    expect(formatActive(59)).toBe("< 1 min");
    expect(formatActive(12 * 60 + 30)).toBe("12 min");
    expect(formatActive(65 * 60)).toBe("1 h 05 min");
  });

  it("com custo, sem preço, estimado e sem dados", () => {
    expect(taskUsageLine(usage("T01", 720, 0.04))).toBe("12 min · US$ 0,04");
    expect(taskUsageLine(usage("T01", 20, null))).toBe("< 1 min");
    expect(taskUsageLine(usage("T01", 720, 0.04, true))).toBe("12 min · US$ 0,04 ~");
    expect(taskUsageLine(usage("T01", 720, 0.0012))).toBe("12 min · US$ 0,0012");
    expect(taskUsageLine({ id: "T01", ...base, cost_usd: 0.5 })).toBe("< 1 min · US$ 0,50");
    expect(taskUsageLine(undefined)).toBeNull();
  });

});

function activity(kind: string, projectId = "p1"): ApiEvent {
  const ev = parseApiEvent({ event_type: "agent_activity", project_id: projectId, payload: { agent: "developer", task_id: "T01", activity_id: "a1", kind, text: "x" } });
  if (ev === null) throw new Error("evento inválido");
  return ev;
}

describe("recarga ao vivo com debounce", () => {
  beforeEach(() => vi.useFakeTimers());

  it("várias atividades em sequência geram uma recarga só", () => {
    const store = createAppStore();
    for (let i = 0; i < 10; i++) store.applyEvent(activity(i % 2 === 0 ? "tool" : "command"));
    expect(store.getState().liveRevisions.p1).toBeUndefined();
    vi.advanceTimersByTime(LIVE_RELOAD_MS - 1);
    expect(store.getState().liveRevisions.p1).toBeUndefined();
    vi.advanceTimersByTime(1);
    expect(store.getState().liveRevisions.p1).toBe(1);
    vi.advanceTimersByTime(LIVE_RELOAD_MS * 3);
    expect(store.getState().liveRevisions.p1).toBe(1);
    // Nova rajada depois da janela: mais uma recarga.
    store.applyEvent(activity("tool"));
    store.applyEvent(activity("end"));
    vi.advanceTimersByTime(LIVE_RELOAD_MS);
    expect(store.getState().liveRevisions.p1).toBe(2);
  });

  it("é por projeto e ignora raciocínio/texto/heartbeat", () => {
    const store = createAppStore();
    store.applyEvent(activity("reasoning"));
    store.applyEvent(activity("text"));
    store.applyEvent(activity("heartbeat"));
    store.applyEvent(activity("tool", "p2"));
    vi.advanceTimersByTime(LIVE_RELOAD_MS);
    expect(store.getState().liveRevisions.p1).toBeUndefined();
    expect(store.getState().liveRevisions.p2).toBe(1);
    // `revisions` (árvore de arquivos etc.) não sobe com a atividade.
    expect(store.getState().revisions.p2).toBeUndefined();
  });

  it("projeto esquecido cancela a recarga pendente", () => {
    const store = createAppStore();
    store.applyEvent(activity("tool"));
    store.forgetProject("p1");
    vi.advanceTimersByTime(LIVE_RELOAD_MS);
    expect(store.getState().liveRevisions.p1).toBeUndefined();
  });
});

describe("uso por tarefa da spec (useUsageView + taskUsageOf)", () => {
  beforeEach(() => resetStore());

  function view(active: number): UsageView {
    return {
      total: { ...base, cost_usd: 0.04 },
      budget: null,
      specs: [{ spec: "s1", ...base, cost_usd: 0.04, tasks: [usage("T01", active, 0.04)], agents: [] }],
      by_agent: [],
      by_model: [],
      by_spec: [],
    };
  }

  it("carrega por spec e recarrega na recarga ao vivo, sem polling", async () => {
    let active = 60;
    const fetchMock = installFetch((_m, url) => (url.includes("/usage?spec=s1") ? { body: view(active) } : { status: 404, body: {} }));
    const { result } = renderHook(() => taskUsageOf(useUsageView("p1", "s1"), "s1"));
    await waitFor(() => expect(result.current.T01?.active_secs).toBe(60));
    const calls = fetchMock.mock.calls.length;
    active = 720;
    act(() => {
      appStore.setState((s) => ({ liveRevisions: { ...s.liveRevisions, p1: (s.liveRevisions.p1 ?? 0) + 1 } }));
    });
    await waitFor(() => expect(result.current.T01?.active_secs).toBe(720));
    expect(fetchMock.mock.calls.length).toBe(calls + 1);
  });

  it("falha de carga não mostra nada", async () => {
    installFetch(() => ({ status: 500, body: { error: { code: "x", message: "falhou" } } }));
    const { result } = renderHook(() => taskUsageOf(useUsageView("p1", "s1"), "s1"));
    await act(async () => {
      await Promise.resolve();
    });
    expect(result.current).toEqual({});
  });
});
