import { describe, expect, it } from "vitest";
import { createAppStore } from "./store";
import type { DialogMessage } from "./api/types";
import { makeProjectState, makeSpec } from "./test-utils";

const PID = "p1";
const BASE = 1_700_000_000_000_000;

function row(i: number, overrides: Partial<DialogMessage> = {}): DialogMessage {
  return { seq: BASE + i, ts: 1_700_000_000 + i, agent: "developer", task_id: "T01", attempt: 1, text: `r${i}`, ...overrides };
}

function texts(store: ReturnType<typeof createAppStore>): string[] {
  return (store.getState().messages[PID] ?? []).filter((m) => m.role === "agent").map((m) => m.text);
}

describe("diálogo com identidade de spec", () => {
  it("merge de seq 5 feito antes de uma carga cujo snapshot vai até 4 mantém o 5", () => {
    const store = createAppStore();
    store.mergeDialog(PID, "s1", [row(5)]);
    store.hydrateDialog(PID, "s1", [row(1), row(2), row(3), row(4)]);

    expect(texts(store)).toEqual(["r1", "r2", "r3", "r4", "r5"]);
  });

  it("linha ao vivo de outra spec (C) permanece na memória e não some com a hidratação vazia de D", () => {
    const store = createAppStore({
      projectStates: { [PID]: makeProjectState({ id: PID, specs: [makeSpec("C", "Antiga", "COMPLETED", [])] }) },
    });
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "de C", seq: BASE + 5, task_id: "T01", spec_id: "C" } });

    store.hydrateDialog(PID, "D", []);

    const kept = (store.getState().messages[PID] ?? []).find((m) => m.text === "de C");
    expect(kept?.specId).toBe("C");
  });

  it("hidratar a spec D não remove as linhas da spec C, e a da D não as substitui", () => {
    const store = createAppStore({
      projectStates: { [PID]: makeProjectState({ id: PID, specs: [makeSpec("C", "Antiga", "COMPLETED", [])] }) },
    });
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "de C", seq: BASE + 5, task_id: "T01", spec_id: "C" } });

    store.hydrateDialog(PID, "D", [row(1)]);

    const all = store.getState().messages[PID] ?? [];
    expect(all.filter((m) => m.specId === "C").map((m) => m.text)).toEqual(["de C"]);
    expect(all.filter((m) => m.specId === "D").map((m) => m.text)).toEqual(["r1"]);
  });

  it("linhas hidratadas trazem a spec da carga", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s9", [row(1)]);

    expect(store.getState().messages[PID]?.[0]?.specId).toBe("s9");
  });
});
