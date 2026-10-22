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

describe("comando sem heartbeat (spec 038)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());

  it("some após 60 s sem heartbeat; heartbeat renova", () => {
    const store = createAppStore();
    const b = { agent: "architect", activity_id: "a1" };
    store.applyEvent(activity({ ...b, kind: "command", text: "x" }));
    vi.advanceTimersByTime(50_000);
    store.applyEvent(activity({ ...b, kind: "heartbeat" }));
    vi.advanceTimersByTime(50_000);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBe("x");
    vi.advanceTimersByTime(11_000);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBeUndefined();
  });
});
