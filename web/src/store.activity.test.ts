import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "./api/events";
import type { ApiEvent } from "./api/types";
import { ACTIVITY_END_GRACE_MS, ACTIVITY_IDLE_MS, COMMAND_STALE_MS, MAX_ACTIVITY_REASONING, createAppStore, mergeServerActivities } from "./store";

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

describe("sinal de vida real do comando e da rodada de testes (spec 123 T01)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());

  it("comando de 9,5 min com heartbeat a cada 20 s: nunca some, e cada sinal traz os bytes e a última linha", () => {
    const store = createAppStore();
    const base = { agent: "developer", task_id: "T03", activity_id: "a1" };
    store.applyEvent(activity({ ...base, kind: "start" }));
    store.applyEvent(activity({ ...base, kind: "tool", text: "rodando npx playwright test --workers=1" }));
    store.applyEvent(activity({ ...base, kind: "command", text: "npx playwright test --workers=1", command_started_at: 1_000_000 }));
    for (let i = 1; i <= 28; i++) {
      // A tela poda a lista entre os sinais, como o relógio da janela faz.
      vi.advanceTimersByTime(10_000);
      store.pruneActivities(P, Date.now());
      vi.advanceTimersByTime(10_000);
      store.applyEvent(activity({ ...base, kind: "heartbeat", output_bytes: i * 512, last_line: `ok ${i} cena.spec.ts` }));
      const a = store.getState().activities[P]?.[0];
      expect(a?.command).toBe("npx playwright test --workers=1");
      expect(a).toMatchObject({ commandBytes: i * 512, commandLine: `ok ${i} cena.spec.ts` });
    }
    // Um heartbeat sem progresso (servidor antigo) mantém o último conhecido.
    store.applyEvent(activity({ ...base, kind: "heartbeat" }));
    expect(store.getState().activities[P]?.[0]).toMatchObject({ commandBytes: 28 * 512, commandLine: "ok 28 cena.spec.ts" });
    store.applyEvent(activity({ ...base, kind: "command_end" }));
    const done = store.getState().activities[P]?.[0];
    expect(done?.command).toBeUndefined();
    expect(done?.commandBytes).toBeUndefined();
    expect(done?.commandLine).toBeUndefined();
  });

  it("sem heartbeat por mais de 60 s o comando some (comportamento de antes, só nesse caso)", () => {
    const store = createAppStore();
    const base = { agent: "developer", task_id: "T03", activity_id: "a1" };
    store.applyEvent(activity({ ...base, kind: "command", text: "npm test", command_started_at: 1_000_000 }));
    store.applyEvent(activity({ ...base, kind: "heartbeat", output_bytes: 10, last_line: "x" }));
    vi.advanceTimersByTime(COMMAND_STALE_MS + 1);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P]?.[0]?.command).toBeUndefined();
    expect(store.getState().activities[P]?.[0]?.commandLine).toBeUndefined();
  });

  it("o heartbeat da rodada de testes atualiza o progresso do agente tests", () => {
    const store = createAppStore();
    const base = { agent: "tests", task_id: "T03", activity_id: "T03-1-4" };
    store.applyEvent(activity({ ...base, kind: "command", text: "npm test", command_started_at: 1_000_000, stage: "Testes · tentativa 1", stage_code: "tests", stage_params: { attempt: 1 } }));
    vi.advanceTimersByTime(20_000);
    store.applyEvent(activity({ ...base, kind: "heartbeat", output_bytes: 2048, last_line: "Tests: 3 passed" }));
    expect(store.getState().activities[P]?.[0]).toMatchObject({ command: "npm test", commandBytes: 2048, commandLine: "Tests: 3 passed", stageCode: { code: "tests", params: { attempt: 1 } } });
  });

  it("reidratação: a rodada de testes vem do instantâneo com o progresso e a etapa; ausente dele, encerra", () => {
    const now = 1_000_000;
    const merged = mergeServerActivities(
      [],
      [
        {
          agent: "tests",
          task_id: "T03",
          spec_id: null,
          activity_id: "T03-1-4",
          reasoning: "",
          text: "",
          tool: "",
          command: "npx playwright test",
          command_started_at: now - 300_000,
          stage: "Testes · tentativa 1",
          stage_code: "tests",
          stage_params: { attempt: 1 },
          output_bytes: 51_200,
          last_line: "ok 7 cena.spec.ts",
          age_ms: 4_000,
        },
      ],
      now,
    );
    expect(merged[0]).toMatchObject({
      agent: "tests",
      command: "npx playwright test",
      commandStartedAt: now - 300_000,
      commandBytes: 51_200,
      commandLine: "ok 7 cena.spec.ts",
      stageCode: { code: "tests", params: { attempt: 1 } },
    });
    // O servidor é a verdade também para a rodada: sem ela no instantâneo, o comando local encerra.
    const later = mergeServerActivities(merged, [], now + 60_000);
    expect(later[0]?.command).toBeUndefined();
  });
});

describe("limite de tempo da rodada de testes (spec 123 T02)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());

  it("o limite vem no início, segue nos sinais de vida e sai no fim; valor inválido é ignorado", () => {
    const store = createAppStore();
    const base = { agent: "tests", task_id: "T03", activity_id: "T03-1-4" };
    store.applyEvent(activity({ ...base, kind: "command", text: "npx playwright test", command_started_at: 1_000_000, command_limit_ms: 600_000 }));
    expect(store.getState().activities[P]?.[0]?.commandLimitMs).toBe(600_000);
    vi.advanceTimersByTime(20_000);
    store.applyEvent(activity({ ...base, kind: "heartbeat", output_bytes: 10, last_line: "ok 1" }));
    expect(store.getState().activities[P]?.[0]?.commandLimitMs).toBe(600_000);
    store.applyEvent(activity({ ...base, kind: "end" }));
    expect(store.getState().activities[P]?.[0]?.commandLimitMs).toBeUndefined();
    // Uma rodada nova sem limite (ou com valor inválido) não herda o anterior.
    store.applyEvent(activity({ ...base, activity_id: "T03-2-5", kind: "command", text: "npm test", command_started_at: 1_020_000, command_limit_ms: -5 }));
    expect(store.getState().activities[P]?.[0]?.commandLimitMs).toBeUndefined();
  });

  it("reidratação: o limite vem do instantâneo", () => {
    const now = 1_000_000;
    const merged = mergeServerActivities(
      [],
      [
        {
          agent: "tests",
          task_id: "T03",
          spec_id: null,
          activity_id: "T03-1-4",
          reasoning: "",
          text: "",
          tool: "",
          command: "npx playwright test",
          command_started_at: now - 420_000,
          command_limit_ms: 600_000,
          age_ms: 4_000,
        },
      ],
      now,
    );
    expect(merged[0]).toMatchObject({ command: "npx playwright test", commandLimitMs: 600_000 });
  });
});

describe("chamada longa ao modelo (spec 114 T04)", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(1_000_000);
  });
  afterEach(() => vi.useRealTimers());
  const base = { agent: "developer", task_id: "T02", activity_id: "a1" };

  it("model marca o início; heartbeats mantêm a entrada viva e trazem os bytes; model_end limpa", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "start" }));
    store.applyEvent(activity({ ...base, kind: "model", model_started_at: 999_000 }));
    expect(store.getState().activities[P]?.[0]).toMatchObject({ modelStartedAt: 999_000 });
    expect(store.getState().activities[P]?.[0]?.modelBytes).toBeUndefined();
    // 8,5 min de chamada: nada de "aguardando a vez" no meio.
    for (let i = 1; i <= 26; i += 1) {
      vi.advanceTimersByTime(20_000);
      store.applyEvent(activity({ ...base, kind: "heartbeat", ...(i > 3 ? { received_bytes: i * 1000 } : {}) }));
      store.pruneActivities(P, Date.now());
    }
    expect(store.getState().activities[P]?.[0]).toMatchObject({ modelStartedAt: 999_000, modelBytes: 26_000 });
    store.applyEvent(activity({ ...base, kind: "model_end" }));
    const a = store.getState().activities[P]?.[0];
    expect(a?.modelStartedAt).toBeUndefined();
    expect(a?.modelBytes).toBeUndefined();
  });

  it("início no futuro (relógio do servidor) fica no agora; bytes inválidos são ignorados", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "model", model_started_at: 5_000_000 }));
    store.applyEvent(activity({ ...base, kind: "heartbeat", received_bytes: -4 }));
    expect(store.getState().activities[P]?.[0]).toMatchObject({ modelStartedAt: 1_000_000 });
    expect(store.getState().activities[P]?.[0]?.modelBytes).toBeUndefined();
  });

  it("sem sinal da chamada, o relógio some; start e end também limpam", () => {
    const store = createAppStore();
    store.applyEvent(activity({ ...base, kind: "model" }));
    vi.advanceTimersByTime(COMMAND_STALE_MS + 1);
    store.pruneActivities(P, Date.now());
    expect(store.getState().activities[P]?.[0]?.modelStartedAt).toBeUndefined();
    store.applyEvent(activity({ ...base, kind: "model" }));
    store.applyEvent(activity({ ...base, kind: "end" }));
    expect(store.getState().activities[P]?.[0]?.modelStartedAt).toBeUndefined();
    store.applyEvent(activity({ ...base, activity_id: "a2", kind: "model" }));
    store.applyEvent(activity({ ...base, activity_id: "a3", kind: "start" }));
    expect(store.getState().activities[P]?.[0]?.modelStartedAt).toBeUndefined();
  });

  it("o instantâneo do servidor traz a chamada em curso (reidratação)", () => {
    const merged = mergeServerActivities(
      [],
      [{ agent: "developer", task_id: "T02", spec_id: "s1", activity_id: "a1", reasoning: "", text: "", tool: "", model_started_at: 900_000, received_bytes: 4096, age_ms: 5_000 }],
      1_000_000,
    );
    expect(merged[0]).toMatchObject({ modelStartedAt: 900_000, modelBytes: 4096 });
  });
});

describe("marco de início ou retomada da execução (spec 114 T04)", () => {
  it("project_status executando grava o instante; parada e outro projeto não mexem", () => {
    const store = createAppStore();
    const status = (payload: Record<string, unknown>, project = P) => {
      const ev = parseApiEvent({ event_type: "project_status", project_id: project, payload });
      if (ev === null) throw new Error("evento inválido");
      store.applyEvent(ev);
    };
    status({ status: "executing", spec_id: "s1" });
    const first = store.getState().runStarts[P];
    expect(first?.seq).toBeGreaterThan(0);
    status({ status: "idle", reason: "stopped" });
    expect(store.getState().runStarts[P]).toBe(first);
    status({ status: "executing", spec_id: "s1" });
    expect(store.getState().runStarts[P]?.seq).toBeGreaterThan(first?.seq ?? 0);
    // Transição vista no estado lido: o marco fica depois de tudo o que já chegou.
    store.markRunStarted(P);
    expect(store.getState().runStarts[P]?.seq).toBeGreaterThan(first?.seq ?? 0);
    expect(store.getState().runStarts.p2).toBeUndefined();
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
