import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "./api/events";
import type { ApiEvent } from "./api/types";
import { ActivityBubbles } from "./apps/shared/ActivityBubble";
import { ACTIVITY_IDLE_MS, createAppStore } from "./store";

const P = "p1";

function ev(event_type: string, payload: Record<string, unknown>): ApiEvent {
  const e = parseApiEvent({ event_type, project_id: P, payload });
  if (e === null) throw new Error("evento inválido");
  return e;
}

const start = {
  agent: "tests",
  task_id: "T01",
  activity_id: "T01-1-0",
  kind: "command",
  text: "npm test",
  stage: "Testes · tentativa 1",
  run_kind: "tests",
  attempt: 1,
  command_started_at: 1_000_000 - 5_000,
};
const end = { agent: "tests", task_id: "T01", activity_id: "T01-1-0", kind: "end" };

describe("testes ao vivo na conversa (spec 071)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("início cria a entrada 'rodando' com o comando e o tempo decorrido", () => {
    const store = createAppStore();
    store.applyEvent(ev("agent_activity", start));
    const list = store.getState().activities[P] ?? [];
    expect(list).toHaveLength(1);
    expect(list[0]?.command).toBe("npm test");
    render(<ActivityBubbles activities={list} />);
    expect(screen.getByTestId("activity-command").textContent).toBe("rodando `npm test` · 0:05");
    expect(screen.getByTestId("activity-stage").textContent).toBe("Testes · tentativa 1");
    act(() => {
      vi.advanceTimersByTime(2000);
    });
    expect(screen.getByTestId("activity-command").textContent).toBe("rodando `npm test` · 0:07");
  });

  it("não expira por inatividade enquanto roda (sem heartbeat)", () => {
    const store = createAppStore();
    store.applyEvent(ev("agent_activity", start));
    vi.setSystemTime(1_000_000 + ACTIVITY_IDLE_MS + 60_000);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBe("npm test");
  });

  it("o resultado persistido substitui a entrada ao vivo, sem duplicar", () => {
    const store = createAppStore();
    store.applyEvent(ev("agent_activity", start));
    store.applyEvent(ev("agent_activity", end));
    store.applyEvent(
      ev("agent_message", { agent: "tests", text: "test_command falhou (1/5)", task_id: "T01", attempt: 1, seq: 7 }),
    );
    expect(store.getState().activities[P] ?? []).toHaveLength(0);
    const msgs = store.getState().messages[P] ?? [];
    expect(msgs.filter((m) => m.text === "test_command falhou (1/5)")).toHaveLength(1);
  });

  it("fim sem mensagem (testes passaram) não deixa bolha vazia", () => {
    const store = createAppStore();
    store.applyEvent(ev("agent_activity", start));
    store.applyEvent(ev("agent_activity", end));
    const list = store.getState().activities[P] ?? [];
    render(<ActivityBubbles activities={list} />);
    expect(screen.queryByTestId("activity-bubble")).toBeNull();
    vi.setSystemTime(1_000_000 + 2_000);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P] ?? []).toHaveLength(0);
  });

  it("reidratação: sem a execução no snapshot do servidor nada é criado; a em curso não é apagada", () => {
    const store = createAppStore();
    store.mergeActivities(P, [], Date.now());
    expect(store.getState().activities[P] ?? []).toHaveLength(0);
    store.applyEvent(ev("agent_activity", start));
    vi.setSystemTime(1_000_000 + 30_000);
    store.mergeActivities(P, [], Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBe("npm test");
  });
});
