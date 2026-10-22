import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, cleanup } from "@testing-library/react";
import { parseApiEvent } from "./api/events";
import { appStore, bumpRevisions, recoveryBadgeFor } from "./store";
import type { ApiEvent } from "./api/types";
import { resetStore } from "./test-utils";

const PID = "p1";

function ev(event_type: string, payload: Record<string, unknown>, project_id: string | null = PID): unknown {
  return { event_type, project_id, payload };
}
function apply(type: string, payload: Record<string, unknown>): void {
  const parsed = parseApiEvent(ev(type, payload));
  if (parsed === null) throw new Error("evento inválido no teste");
  act(() => appStore.applyEvent(parsed as ApiEvent));
}

describe("parser dos eventos de recuperação", () => {
  it("aceita recovery_started e recovery_decided válidos", () => {
    expect(parseApiEvent(ev("recovery_started", { spec_id: "s1", task_id: "T01", cycle: 1 }))).not.toBeNull();
    expect(
      parseApiEvent(ev("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 2, decision: "ESCALATE" })),
    ).not.toBeNull();
    expect(
      parseApiEvent(ev("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 2, decision: "RETRY" })),
    ).not.toBeNull();
  });
  it("ignora inválidos", () => {
    expect(parseApiEvent(ev("recovery_started", { spec_id: "s1", task_id: "T01", cycle: "1" }))).toBeNull();
    expect(parseApiEvent(ev("recovery_started", { spec_id: "s1", cycle: 1 }))).toBeNull();
    expect(parseApiEvent(ev("recovery_started", { spec_id: "s1", task_id: "T01", cycle: -1 }))).toBeNull();
    expect(
      parseApiEvent(ev("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "MAYBE" })),
    ).toBeNull();
    expect(parseApiEvent(ev("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1 }))).toBeNull();
  });
  it("kind em blocked: válido mantido, inválido descartado, ausente tolerado", () => {
    const base = { status: "blocked", task_id: "T01", reason: "r" };
    const ok = parseApiEvent(ev("project_status", { ...base, kind: "environmental" }));
    expect((ok?.payload as { kind?: string }).kind).toBe("environmental");
    const bad = parseApiEvent(ev("project_status", { ...base, kind: "xyz" }));
    expect(bad).not.toBeNull();
    expect((bad?.payload as { kind?: string }).kind).toBeUndefined();
    expect(parseApiEvent(ev("project_status", base))).not.toBeNull();
  });
});

describe("selos de recuperação do Quadro por fase", () => {
  beforeEach(() => resetStore());
  afterEach(cleanup);

  it("analisando até o decided; RETRY vira estratégia nova até sair de TODO/IN_PROGRESS", () => {
    expect(recoveryBadgeFor(appStore.getState(), PID, "s1", "T01")).toBeNull();
    apply("recovery_started", { spec_id: "s1", task_id: "T01", cycle: 1 });
    expect(recoveryBadgeFor(appStore.getState(), PID, "s1", "T01")).toEqual({ phase: "analyzing", cycle: 1 });
    apply("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "RETRY" });
    expect(recoveryBadgeFor(appStore.getState(), PID, "s1", "T01")).toEqual({ phase: "retry", cycle: 1 });
    apply("task_updated", { spec_id: "s1", task_id: "T01", status: "IN_PROGRESS" });
    expect(recoveryBadgeFor(appStore.getState(), PID, "s1", "T01")?.phase).toBe("retry");
    apply("task_updated", { spec_id: "s1", task_id: "T01", status: "IN_REVIEW" });
    expect(recoveryBadgeFor(appStore.getState(), PID, "s1", "T01")).toBeNull();
  });

  it("ESCALATE não gera selo; não vaza entre specs", () => {
    apply("recovery_started", { spec_id: "s1", task_id: "T01", cycle: 1 });
    expect(recoveryBadgeFor(appStore.getState(), PID, "s2", "T01")).toBeNull();
    apply("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "ESCALATE" });
    expect(recoveryBadgeFor(appStore.getState(), PID, "s1", "T01")).toBeNull();
  });

});

describe("selo analisando preso", () => {
  beforeEach(() => resetStore());
  const badge = () => recoveryBadgeFor(appStore.getState(), PID, "s1", "T01");
  const start = () => apply("recovery_started", { spec_id: "s1", task_id: "T01", cycle: 1 });

  it("status terminal sem decided limpa o selo", () => {
    start();
    expect(badge()?.phase).toBe("analyzing");
    apply("project_status", { status: "idle", reason: "shutdown" });
    expect(badge()).toBeNull();
  });
  it("task_updated para status diferente de BLOCKED limpa; BLOCKED mantém", () => {
    start();
    apply("task_updated", { spec_id: "s1", task_id: "T01", status: "BLOCKED" });
    expect(badge()?.phase).toBe("analyzing");
    apply("task_updated", { spec_id: "s1", task_id: "T01", status: "TODO" });
    expect(badge()).toBeNull();
  });
  it("decided mantém o comportamento atual", () => {
    start();
    apply("recovery_decided", { spec_id: "s1", task_id: "T01", cycle: 1, decision: "RETRY" });
    expect(badge()?.phase).toBe("retry");
  });
  it("lagged/reconexão limpa analyzing", () => {
    start();
    bumpRevisions();
    expect(badge()).toBeNull();
  });
});
