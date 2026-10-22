import { beforeEach, describe, expect, it } from "vitest";
import { act } from "@testing-library/react";
import { parseApiEvent } from "./api/events";
import { appStore, isFinalVerificationActive, mergeServerActivities, pruneActivities } from "./store";
import type { ApiEvent } from "./api/types";
import { resetStore } from "./test-utils";

const PID = "p1";

function activity(payload: Record<string, unknown>): ApiEvent {
  const ev = parseApiEvent({ event_type: "agent_activity", project_id: PID, payload });
  if (ev === null) throw new Error("evento inválido");
  return ev;
}

const START = { agent: "reviewer", activity_id: "f1", kind: "start", stage: "final_verification" };

describe("verificação final: estado", () => {
  beforeEach(() => resetStore());

  it("fica ativa até o end e não vence por ociosidade", () => {
    expect(isFinalVerificationActive(appStore.getState().activities[PID])).toBe(false);
    act(() => appStore.applyEvent(activity(START)));
    const list = appStore.getState().activities[PID] ?? [];
    expect(isFinalVerificationActive(list)).toBe(true);
    expect(pruneActivities(list, Date.now() + 60 * 60_000)).toHaveLength(1);
    act(() => appStore.applyEvent(activity({ ...START, kind: "end" })));
    expect(isFinalVerificationActive(appStore.getState().activities[PID])).toBe(false);
  });
});

describe("verificação final: snapshot do servidor", () => {
  const snap = (stage: string | null) => ({
    agent: "reviewer",
    task_id: null,
    spec_id: "s1",
    activity_id: "f1",
    reasoning: "",
    text: "",
    tool: "",
    stage,
    age_ms: 10 * 60_000,
  });

  it("mantém a verificação final com 10 min de idade e ativa o indicador", () => {
    const merged = mergeServerActivities([], [snap("final_verification")], 5_000_000);
    expect(merged).toHaveLength(1);
    expect(isFinalVerificationActive(merged)).toBe(true);
    expect(pruneActivities(merged, 5_000_000)).toHaveLength(1);
  });

  it("outras atividades antigas continuam descartadas", () => {
    expect(mergeServerActivities([], [snap(null)], 5_000_000)).toHaveLength(0);
  });
});
