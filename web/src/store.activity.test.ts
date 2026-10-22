import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "./api/events";
import type { ApiEvent } from "./api/types";
import { ACTIVITY_END_GRACE_MS, ACTIVITY_IDLE_MS, MAX_ACTIVITY_REASONING, createAppStore } from "./store";

const P = "p1";

function activity(payload: Record<string, unknown>, projectId: string | null = P): ApiEvent {
  const ev = parseApiEvent({ event_type: "agent_activity", project_id: projectId, payload });
  if (ev === null) throw new Error("evento inválido");
  return ev;
}

function agentMessage(agent: string, taskId?: string): ApiEvent {
  const ev = parseApiEvent({
    event_type: "agent_message",
    project_id: P,
    payload: { agent, text: "pronto", ...(taskId ? { task_id: taskId, spec_id: "s1" } : {}) },
  });
  if (ev === null) throw new Error("evento inválido");
  return ev;
}

describe("agent_activity (efêmero)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());

  it("parse: kind desconhecido é inválido", () => {
    expect(parseApiEvent({ event_type: "agent_activity", project_id: P, payload: { agent: "developer", activity_id: "a", kind: "x" } })).toBeNull();
  });

  it("start/reasoning/tool/text/end: snapshots substituem, não concatenam", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "start" }));
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "reasoning", text: "abc" }));
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "reasoning", text: "abcdef" }));
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "tool", text: "lendo src/x.rs" }));
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "text", text: "rasc" }));
    const [a] = store.getState().activities[P] ?? [];
    expect(a).toMatchObject({ agent: "developer", taskId: "T01", reasoning: "abcdef", tool: "lendo src/x.rs", text: "rasc" });
    expect(a?.endedAt).toBeUndefined();
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "end" }));
    expect(store.getState().activities[P]?.[0]?.endedAt).toBe(1_000_000);
  });

  it("passo seguinte do mesmo agente/tarefa reaproveita a bolha (sem piscar)", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "reasoning", text: "um" }));
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "end" }));
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a2", kind: "start" }));
    const list = store.getState().activities[P] ?? [];
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({ activityId: "a2", reasoning: "" });
    expect(list[0]?.endedAt).toBeUndefined();
  });

  it("end de passo antigo não encerra o passo atual", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", activity_id: "a2", kind: "start" }));
    store.applyEvent(activity({ agent: "developer", activity_id: "a1", kind: "end" }));
    expect(store.getState().activities[P]?.[0]?.endedAt).toBeUndefined();
  });

  it("teto do reasoning mantém o fim", () => {
    const store = createAppStore();
    const big = "x".repeat(MAX_ACTIVITY_REASONING) + "FIM";
    store.applyEvent(activity({ agent: "developer", activity_id: "a1", kind: "reasoning", text: big }));
    const r = store.getState().activities[P]?.[0]?.reasoning ?? "";
    expect(r.length).toBe(MAX_ACTIVITY_REASONING);
    expect(r.endsWith("FIM")).toBe(true);
  });

  it("carência após end e inatividade removem a bolha", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", activity_id: "a1", kind: "start" }));
    store.applyEvent(activity({ agent: "developer", activity_id: "a1", kind: "end" }));
    store.pruneActivities(P, 1_000_000 + ACTIVITY_END_GRACE_MS - 1);
    expect(store.getState().activities[P]).toHaveLength(1);
    store.pruneActivities(P, 1_000_000 + ACTIVITY_END_GRACE_MS);
    expect(store.getState().activities[P]).toHaveLength(0);
    store.applyEvent(activity({ agent: "reviewer", activity_id: "r1", kind: "start" }));
    store.pruneActivities(P, 1_000_000 + ACTIVITY_IDLE_MS);
    expect(store.getState().activities[P]).toHaveLength(0);
  });

  it("mensagem final do mesmo agente/tarefa substitui a bolha; outras ficam", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "start" }));
    store.applyEvent(activity({ agent: "reviewer", task_id: "T01", activity_id: "r1", kind: "start" }));
    store.applyEvent(agentMessage("developer", "T01"));
    expect(store.getState().activities[P]?.map((a) => a.agent)).toEqual(["reviewer"]);
  });

  it("resposta do Arquiteto limpa a bolha dele; fim do run limpa todas", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "architect", activity_id: "x1", kind: "start" }));
    store.applyEvent(activity({ agent: "developer", activity_id: "d1", kind: "start" }));
    store.showArchitectReply(P, "ok", "t".repeat(32));
    expect(store.getState().activities[P]?.map((a) => a.agent)).toEqual(["developer"]);
    store.applyEvent({ event_type: "project_status", project_id: P, payload: { status: "idle", reason: "pausa" } });
    expect(store.getState().activities[P]).toEqual([]);
  });

  it("nunca hidrata: forgetProject descarta", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", activity_id: "d1", kind: "start" }));
    store.forgetProject(P);
    expect(store.getState().activities[P]).toBeUndefined();
  });
});

describe("comando longo (heartbeat)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());

  it("heartbeats mantêm a bolha além de ACTIVITY_IDLE_MS; command_end limpa", () => {
    const store = createAppStore();
    const base = { agent: "developer", task_id: "T01", activity_id: "a1" };
    store.applyEvent(activity({ ...base, kind: "command", text: "npm test", command_started_at: 1_000_000 }));
    expect(store.getState().activities[P]?.[0]).toMatchObject({ command: "npm test", commandStartedAt: 1_000_000 });
    for (let i = 0; i < 10; i++) {
      vi.advanceTimersByTime(20_000);
      store.applyEvent(activity({ ...base, kind: "heartbeat" }));
    }
    store.pruneActivities(P, Date.now());
    expect(Date.now() - 1_000_000).toBeGreaterThan(ACTIVITY_IDLE_MS);
    expect(store.getState().activities[P]).toHaveLength(1);
    expect(store.getState().activities[P]?.[0]?.command).toBe("npm test");
    store.applyEvent(activity({ ...base, kind: "command_end" }));
    expect(store.getState().activities[P]?.[0]?.command).toBeUndefined();
  });

  it("sem heartbeat a bolha expira", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "developer", activity_id: "a1", kind: "command", text: "x" }));
    vi.advanceTimersByTime(ACTIVITY_IDLE_MS + 1);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P] ?? []).toHaveLength(0);
  });
});

describe("etapa e fim da finalização (spec 047)", () => {
  it("stage do evento chega à entrada; start sem stage limpa", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "architect", activity_id: "a1", kind: "start", stage: "Atualizando ARCHITECTURE.md" }));
    store.applyEvent(activity({ agent: "architect", activity_id: "a1", kind: "tool", text: "buscando x", stage: "Atualizando ARCHITECTURE.md" }));
    expect(store.getState().activities[P]?.[0]?.stage).toBe("Atualizando ARCHITECTURE.md");
    store.applyEvent(activity({ agent: "architect", activity_id: "a2", kind: "start" }));
    expect(store.getState().activities[P]?.[0]?.stage).toBeUndefined();
  });

  it("finalization_done recarrega o estado (revisions sobe)", () => {
    const store = createAppStore();
    const ev = parseApiEvent({ event_type: "finalization_done", project_id: P, payload: { spec_id: "s1" } });
    expect(ev).not.toBeNull();
    const before = store.getState().revisions[P] ?? 0;
    if (ev !== null) store.applyEvent(ev);
    expect(store.getState().revisions[P]).toBe(before + 1);
    expect(parseApiEvent({ event_type: "finalization_done", project_id: P, payload: {} })).toBeNull();
  });

  it("report_ready encerra a bolha do Arquiteto sem tarefa", () => {
    const store = createAppStore();
    store.applyEvent(activity({ agent: "architect", activity_id: "a1", kind: "reasoning", text: "escrevendo" }));
    expect(store.getState().activities[P] ?? []).toHaveLength(1);
    const ready = parseApiEvent({ event_type: "report_ready", project_id: P, payload: { spec_id: "s1" } });
    if (ready === null) throw new Error("evento inválido");
    store.applyEvent(ready);
    expect(store.getState().activities[P] ?? []).toHaveLength(0);
  });
});
