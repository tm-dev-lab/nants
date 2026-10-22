import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "./api/events";
import type { ApiEvent } from "./api/types";
import { createAppStore } from "./store";

const P = "p1";

function activity(payload: Record<string, unknown>): ApiEvent {
  const ev = parseApiEvent({ event_type: "agent_activity", project_id: P, payload });
  if (ev === null) throw new Error("evento inválido");
  return ev;
}

describe("comando preso (spec 038)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());

  const base = { agent: "architect", activity_id: "a1" };

  it("evento de outro passo encerra o comando (command_end perdido)", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "command", text: "ls -la" }));
    store.applyEvent(activity({ ...base, activity_id: "a2", kind: "tool", text: "escrevendo artefatos" }));
    const e = store.getState().activities[P]?.[0];
    expect(e?.command).toBeUndefined();
    expect(e?.commandStartedAt).toBeUndefined();
    expect(e?.tool).toBe("escrevendo artefatos");
  });

  it("end do mesmo passo encerra o comando", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "command", text: "x" }));
    store.applyEvent(activity({ ...base, kind: "end" }));
    expect(store.getState().activities[P]?.[0]?.command).toBeUndefined();
  });

  it("evento do mesmo passo mantém o comando", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "command", text: "x" }));
    store.applyEvent(activity({ ...base, kind: "text", text: "oi" }));
    expect(store.getState().activities[P]?.[0]?.command).toBe("x");
  });

  it("reidratação: servidor sem comando (ou sem a entrada) encerra o comando local", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "command", text: "x", command_started_at: 1_000_000 }));
    vi.advanceTimersByTime(30_000);
    store.applyEvent(activity({ ...base, kind: "heartbeat" }));
    store.mergeActivities(
      P,
      [{ agent: "architect", task_id: null, spec_id: null, activity_id: "a1", reasoning: "", text: "", tool: "", command: null, age_ms: 1_000 }],
      Date.now(),
    );
    expect(store.getState().activities[P]?.[0]?.command).toBeUndefined();

    store.applyEvent(activity({ ...base, activity_id: "a3", kind: "command", text: "y", command_started_at: Date.now() }));
    vi.advanceTimersByTime(10_000);
    store.mergeActivities(P, [], Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBeUndefined();
  });

  it("reidratação com instantâneo antigo não apaga comando recém-iniciado", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "command", text: "x", command_started_at: Date.now() }));
    store.mergeActivities(P, [], Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBe("x");
  });
});
