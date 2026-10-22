import { describe, expect, it } from "vitest";
import { createAppStore } from "./store";
import type { DialogMessage } from "./api/types";
import { makeProjectState, makeSpec } from "./test-utils";

const PID = "p1";
const BASE = 1_700_000_000_000_000;

function row(i: number, text = `r${i}`): DialogMessage {
  return { seq: BASE + i, ts: 1_700_000_000 + i, agent: "developer", task_id: "T01", attempt: 1, text };
}

describe("identidade de spec no diálogo ao vivo", () => {
  it("evento com spec_id da spec nova, com o estado ainda na anterior: aparece na nova e não na antiga", () => {
    const store = createAppStore({
      projectStates: { [PID]: makeProjectState({ id: PID, specs: [makeSpec("old", "Antiga", "COMPLETED", [])] }) },
    });
    store.applyEvent({
      event_type: "agent_message",
      project_id: PID,
      payload: { agent: "developer", text: "primeira da nova", seq: BASE + 9, task_id: "T01", spec_id: "new" },
    });

    const msg = (store.getState().messages[PID] ?? []).find((m) => m.text === "primeira da nova");
    expect(msg?.specId).toBe("new");
  });

  it("evento sem spec_id e sem estado do projeto: marcado como sem spec, com a spec ativa da chegada ausente", () => {
    const store = createAppStore();
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "sem dono" } });

    const msg = (store.getState().messages[PID] ?? []).find((m) => m.text === "sem dono");
    expect(msg?.specId).toBeUndefined();
    expect(msg?.unscoped).toBe(true);
    expect(msg?.arrivedUnderSpec).toBeUndefined();
  });

  it("evento sem spec_id guarda a spec ativa do instante da chegada", () => {
    const store = createAppStore({
      projectStates: { [PID]: makeProjectState({ id: PID, specs: [makeSpec("s1", "Plano", "IN_PROGRESS", [])] }) },
    });
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "antiga" } });

    const msg = (store.getState().messages[PID] ?? []).find((m) => m.text === "antiga");
    expect(msg?.arrivedUnderSpec).toBe("s1");
  });

  it("a hidratação de uma spec descarta as linhas sem spec", () => {
    const store = createAppStore();
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "sem dono" } });

    store.hydrateDialog(PID, "s1", [row(1)]);

    const texts = (store.getState().messages[PID] ?? []).map((m) => m.text);
    expect(texts).toEqual(["r1"]);
  });
});

describe("dedupe e recuperação por (spec, seq)", () => {
  it("recuperação com o mesmo dialogSeq em specs diferentes: as duas aparecem", () => {
    const store = createAppStore();
    store.mergeDialog(PID, "s1", [row(5, "de s1")]);
    store.mergeDialog(PID, "s2", [row(5, "de s2")]);

    const texts = (store.getState().messages[PID] ?? []).map((m) => m.text).sort();
    expect(texts).toEqual(["de s1", "de s2"]);
  });

  it("evento ao vivo com seq já presente na MESMA spec não duplica; em outra spec, entra", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(5, "hist s1")]);
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "ao vivo s1", seq: BASE + 5, spec_id: "s1" } });
    store.applyEvent({ event_type: "agent_message", project_id: PID, payload: { agent: "developer", text: "ao vivo s2", seq: BASE + 5, spec_id: "s2" } });

    const texts = (store.getState().messages[PID] ?? []).map((m) => m.text).sort();
    expect(texts).toEqual(["ao vivo s2", "hist s1"]);
  });
});
