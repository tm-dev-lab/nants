import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  EventStreamClient,
  eventStreamUrl,
  parseApiEvent,
  parseEventData,
  parseLagged,
  reconnectDelay,
  type EventSourceLike,
} from "./events";

/** EventSource falso: registra listeners e permite disparar eventos. */
class FakeSource implements EventSourceLike {
  readyState = 0;
  onerror: ((ev: Event) => void) | null = null;
  onopen: ((ev: Event) => void) | null = null;
  closed = false;
  listeners = new Map<string, Array<(ev: MessageEvent<string>) => void>>();

  addEventListener(type: string, listener: (ev: MessageEvent<string>) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }

  close(): void {
    this.closed = true;
    this.readyState = 2;
  }

  emit(type: string, data: string): void {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(new MessageEvent(type, { data }));
    }
  }
}

describe("validação de eventos", () => {
  it("aceita envelope conhecido com payload objeto", () => {
    const ev = parseApiEvent({
      event_type: "agent_message",
      project_id: "p1",
      payload: { agent: "architect", text: "oi" },
    });
    expect(ev?.event_type).toBe("agent_message");
    expect(ev?.project_id).toBe("p1");
  });

  it("ignora event_type desconhecido", () => {
    expect(
      parseApiEvent({ event_type: "coisa_nova", project_id: null, payload: {} }),
    ).toBeNull();
  });

  it("ignora payload que não é objeto", () => {
    expect(parseApiEvent({ event_type: "task_updated", project_id: "p1", payload: [1] })).toBeNull();
    expect(parseApiEvent({ event_type: "task_updated", project_id: "p1", payload: "x" })).toBeNull();
    expect(parseApiEvent({ event_type: "task_updated", project_id: "p1", payload: null })).toBeNull();
  });

  it("valida o payload por tipo: payload vazio ou com tipos errados vira null", () => {
    const invalid: Array<[string, unknown]> = [
      ["project_status", {}],
      ["project_status", { status: "planning" }],
      ["project_status", { status: "executing" }],
      ["project_status", { status: "executing", spec_id: 5 }],
      ["project_status", { status: "integrated" }],
      ["project_status", { status: "blocked", task_id: "T01" }],
      ["project_status", { status: "blocked", task_id: 1, reason: "x" }],
      ["project_status", { status: "idle" }],
      ["project_status", { status: "idle", reason: "x", needs_manual_recovery: "sim" }],
      ["task_updated", {}],
      ["task_updated", { spec_id: "005", task_id: "T01" }],
      ["task_updated", { spec_id: "005", task_id: "T01", status: "FEITO" }],
      ["task_updated", { spec_id: "005", task_id: "T01", status: "DONE", integrated: "sim" }],
      ["agent_message", {}],
      ["agent_message", { agent: "architect" }],
      ["agent_message", { agent: 1, text: "x" }],
      ["permission_requested", {}],
      ["permission_requested", { request_id: "r1", project_id: "p1", agent: "developer", command: "ls" }],
      ["permission_requested", { request_id: "r1", project_id: "p1", task_id: "T01", agent: "developer", command: 3 }],
      ["sdd_draft_updated", {}],
      ["sdd_draft_updated", { project_id: 9 }],
    ];
    for (const [event_type, payload] of invalid) {
      expect(parseApiEvent({ event_type, project_id: "p1", payload }), `${event_type} ${JSON.stringify(payload)}`).toBeNull();
    }
  });

  it("aceita o payload válido de cada tipo", () => {
    const valid: Array<[string, unknown]> = [
      ["project_status", { status: "executing", spec_id: "005" }],
      ["project_status", { status: "integrated", spec_id: "005" }],
      ["project_status", { status: "blocked", task_id: "T01", reason: "task_blocked" }],
      ["project_status", { status: "idle", reason: "error" }],
      ["project_status", { status: "idle", reason: "restore_failed", needs_manual_recovery: true }],
      ["task_updated", { spec_id: "005", task_id: "T01", status: "DONE", integrated: true }],
      ["task_updated", { spec_id: "005", task_id: "T01", status: "IN_REVIEW" }],
      ["agent_message", { agent: "architect", text: "oi" }],
      ["permission_requested", { request_id: "r1", project_id: "p1", task_id: "T01", agent: "developer", command: "ls" }],
      ["sdd_draft_updated", { project_id: "p1" }],
    ];
    for (const [event_type, payload] of valid) {
      expect(parseApiEvent({ event_type, project_id: "p1", payload }), event_type).not.toBeNull();
    }
  });

  it("ignora project_id com tipo inválido", () => {
    expect(parseApiEvent({ event_type: "sdd_draft_updated", project_id: 7, payload: {} })).toBeNull();
  });

  it("ignora JSON inválido e valores que não são objeto", () => {
    expect(parseEventData("{não é json")).toBeNull();
    expect(parseEventData("[]")).toBeNull();
    expect(parseEventData("null")).toBeNull();
  });

  it("decodifica lagged com skipped numérico", () => {
    expect(parseLagged('{"skipped": 12}')).toEqual({ skipped: 12 });
    expect(parseLagged('{"skipped": "12"}')).toBeNull();
    expect(parseLagged('{"skipped": -1}')).toBeNull();
    expect(parseLagged("oi")).toBeNull();
  });

  it("monta URL com project_id codificado", () => {
    expect(eventStreamUrl()).toBe("/api/events");
    expect(eventStreamUrl("p-1")).toBe("/api/events?project_id=p-1");
    expect(eventStreamUrl("a&b")).toBe("/api/events?project_id=a%26b");
  });

  it("reconexão cresce de 1 s até o teto de 15 s", () => {
    const delays = [0, 1, 2, 3, 4, 5, 10].map((n) => reconnectDelay(n));
    expect(delays).toEqual([1000, 2000, 4000, 8000, 15000, 15000, 15000]);
  });
});

describe("EventStreamClient", () => {
  let sources: FakeSource[];
  let factory: (url: string) => EventSourceLike;

  beforeEach(() => {
    vi.useFakeTimers();
    sources = [];
    factory = () => {
      const s = new FakeSource();
      sources.push(s);
      return s;
    };
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("entrega eventos válidos e ignora desconhecidos e lagged mal formado", () => {
    const onEvent = vi.fn();
    const onLagged = vi.fn();
    const client = new EventStreamClient({
      projectId: "p1",
      onEvent,
      onLagged,
      createEventSource: factory,
    });
    client.start();
    const src = sources[0];
    if (!src) throw new Error("fonte não criada");
    src.emit(
      "task_updated",
      JSON.stringify({
        event_type: "task_updated",
        project_id: "p1",
        payload: { spec_id: "005", task_id: "T01", status: "DONE" },
      }),
    );
    src.emit("task_updated", "{quebrado");
    src.emit("lagged", '{"skipped": 3}');
    src.emit("lagged", "nada");
    expect(onEvent).toHaveBeenCalledTimes(1);
    expect(onLagged).toHaveBeenCalledTimes(1);
    expect(onLagged).toHaveBeenCalledWith({ skipped: 3 });
    client.close();
  });

  it("reconecta com atraso exponencial e close() cancela tentativas pendentes", () => {
    const client = new EventStreamClient({
      onEvent: () => {},
      createEventSource: factory,
    });
    client.start();
    expect(sources).toHaveLength(1);

    sources[0]?.onerror?.(new Event("error"));
    expect(sources[0]?.closed).toBe(true);
    vi.advanceTimersByTime(999);
    expect(sources).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(sources).toHaveLength(2);

    sources[1]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(1999);
    expect(sources).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(sources).toHaveLength(3);

    client.close();
    vi.advanceTimersByTime(60_000);
    expect(sources).toHaveLength(3);
  });

  it("onopen não zera a sequência: conexão que cai antes de 10 s mantém o backoff crescendo", () => {
    const client = new EventStreamClient({ onEvent: () => {}, createEventSource: factory });
    client.start();
    sources[0]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(1000);
    sources[1]?.onopen?.(new Event("open"));
    vi.advanceTimersByTime(9_999);
    sources[1]?.onerror?.(new Event("error"));
    // Próxima tentativa após 2 s (não 1 s): a abertura curta não resetou o atraso.
    vi.advanceTimersByTime(1999);
    expect(sources).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(sources).toHaveLength(3);
    sources[2]?.onopen?.(new Event("open"));
    vi.advanceTimersByTime(9_999);
    sources[2]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(3999);
    expect(sources).toHaveLength(3);
    vi.advanceTimersByTime(1);
    expect(sources).toHaveLength(4);
    client.close();
  });

  it("conexão aberta por 10 s ou mais zera a sequência de tentativas", () => {
    const client = new EventStreamClient({ onEvent: () => {}, createEventSource: factory });
    client.start();
    sources[0]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(1000);
    sources[1]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(2000);
    sources[2]?.onopen?.(new Event("open"));
    vi.advanceTimersByTime(10_000);
    sources[2]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(999);
    expect(sources).toHaveLength(3);
    vi.advanceTimersByTime(1);
    expect(sources).toHaveLength(4);
    client.close();
  });

  it("close() fecha a fonte atual e reporta status closed", () => {
    const statuses: string[] = [];
    const client = new EventStreamClient({
      onEvent: () => {},
      onStatus: (s) => statuses.push(s),
      createEventSource: factory,
    });
    client.start();
    client.close();
    client.close();
    expect(sources[0]?.closed).toBe(true);
    expect(statuses.at(-1)).toBe("closed");
  });

  it("não processa eventos de uma fonte já descartada", () => {
    const onEvent = vi.fn();
    const client = new EventStreamClient({ onEvent, createEventSource: factory });
    client.start();
    const old = sources[0];
    old?.onerror?.(new Event("error"));
    old?.emit(
      "sdd_draft_updated",
      JSON.stringify({ event_type: "sdd_draft_updated", project_id: "p", payload: {} }),
    );
    expect(onEvent).not.toHaveBeenCalled();
    client.close();
  });
});

describe("permission_requested e permission_resolved: escopos", () => {
  const base = { request_id: "r1", project_id: "p1", task_id: null, agent: "developer", command: "node --test" };

  it("aceita always_allowed false com motivo e true com motivo null", () => {
    const denied = { ...base, always_allowed: false, always_denied_reason: "este comando executa código arbitrário", always_denied_code: "arbitrary" };
    const allowed = { ...base, command: "git status", always_allowed: true, always_denied_reason: null };
    expect(parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: denied })).not.toBeNull();
    expect(parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: allowed })).not.toBeNull();
  });

  it("aceita pedido sem os campos de escopo (servidor antigo)", () => {
    expect(parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: base })).not.toBeNull();
  });

  it("rejeita always_allowed de tipo errado", () => {
    expect(
      parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: { ...base, always_allowed: "sim" } }),
    ).toBeNull();
  });

  it("rejeita always_denied_code de tipo errado (spec 115 T03)", () => {
    expect(
      parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: { ...base, always_allowed: false, always_denied_code: 3 } }),
    ).toBeNull();
  });

  it("aceita permission_resolved com applied e rejeita sem request_id", () => {
    const payload = { request_id: "r1", project_id: "p1", applied: "run" };
    expect(parseApiEvent({ event_type: "permission_resolved", project_id: "p1", payload })).not.toBeNull();
    expect(
      parseApiEvent({ event_type: "permission_resolved", project_id: "p1", payload: { ...payload, request_id: "" } }),
    ).toBeNull();
  });
});

describe("permission_requested: task_id nulo ou string", () => {
  const base = { request_id: "r1", project_id: "p1", agent: "developer", command: "cargo test" };

  it("aceita task_id null (comando fora da allowlist, sem tarefa associada)", () => {
    expect(
      parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: { ...base, task_id: null } }),
    ).not.toBeNull();
  });

  it("aceita task_id string", () => {
    expect(
      parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: { ...base, task_id: "T01" } }),
    ).not.toBeNull();
  });

  it("rejeita task_id ausente (o servidor sempre serializa a chave)", () => {
    expect(parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: base })).toBeNull();
  });

  it("rejeita request_id e command vazios (o servidor nunca os emite)", () => {
    for (const patch of [{ request_id: "" }, { command: "" }]) {
      expect(
        parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: { ...base, task_id: null, ...patch } }),
        JSON.stringify(patch),
      ).toBeNull();
    }
  });

  it("rejeita task_id numérico", () => {
    expect(
      parseApiEvent({ event_type: "permission_requested", project_id: "p1", payload: { ...base, task_id: 1 } }),
    ).toBeNull();
  });
});

/**
 * Contrato: payloads EXATOS que o servidor emite (copiados de runs.rs, events.rs,
 * approvals.rs e chat.rs). Se o servidor mudar, este teste precisa refletir a mudança.
 */
describe("contrato: payloads reais do servidor são aceitos", () => {
  const SERVER_EVENTS: Array<{ source: string; event: unknown }> = [
    // runs.rs: status_event(executing) e integrated
    { source: "runs.rs executing", event: { event_type: "project_status", project_id: "p1", payload: { status: "executing", spec_id: "005" } } },
    { source: "runs.rs integrated", event: { event_type: "project_status", project_id: "p1", payload: { status: "integrated", spec_id: "005" } } },
    // runs.rs: blocked (task_id + reason)
    { source: "runs.rs blocked", event: { event_type: "project_status", project_id: "p1", payload: { status: "blocked", task_id: "T02", reason: "task_blocked" } } },
    // runs.rs: idle com reason e idle de restore_failed com needs_manual_recovery
    { source: "runs.rs idle", event: { event_type: "project_status", project_id: "p1", payload: { status: "idle", reason: "stalled" } } },
    { source: "runs.rs idle restore_failed", event: { event_type: "project_status", project_id: "p1", payload: { status: "idle", reason: "restore_failed", needs_manual_recovery: true } } },
    // events.rs: task_updated normal e integrated (integrated: true)
    { source: "events.rs task_updated", event: { event_type: "task_updated", project_id: "p1", payload: { spec_id: "005", task_id: "T01", status: "IN_PROGRESS" } } },
    { source: "events.rs task_updated integrated", event: { event_type: "task_updated", project_id: "p1", payload: { spec_id: "005", task_id: "T01", status: "DONE", integrated: true } } },
    // events.rs: agent_message
    { source: "events.rs agent_message", event: { event_type: "agent_message", project_id: "p1", payload: { agent: "architect", text: "oi" } } },
    // approvals.rs: permission_requested com task_id null e com string
    { source: "approvals.rs permission null", event: { event_type: "permission_requested", project_id: "p1", payload: { request_id: "r1", project_id: "p1", task_id: null, agent: "developer", command: "cargo test" } } },
    { source: "approvals.rs permission string", event: { event_type: "permission_requested", project_id: "p1", payload: { request_id: "r2", project_id: "p1", task_id: "T04", agent: "developer", command: "ls" } } },
    // chat.rs: sdd_draft_updated
    { source: "chat.rs sdd_draft_updated", event: { event_type: "sdd_draft_updated", project_id: "p1", payload: { project_id: "p1" } } },
  ];

  for (const { source, event } of SERVER_EVENTS) {
    it(`aceita ${source}`, () => {
      expect(parseApiEvent(event)).not.toBeNull();
    });
  }
});

describe("kb_updated (spec 134 T02)", () => {
  it("aceita mudança de estado e remoção, recusa payload fora do contrato", () => {
    const ready = { id: "a-1", status: "no_text", status_reason: "kb_pdf_no_text", doc: { id: "a-1" } };
    expect(parseApiEvent({ event_type: "kb_updated", project_id: "p", payload: ready })).not.toBeNull();
    const removed = { id: "a-1", removed: true };
    expect(parseApiEvent({ event_type: "kb_updated", project_id: "p", payload: removed })).not.toBeNull();
    expect(parseApiEvent({ event_type: "kb_updated", project_id: "p", payload: { id: "a-1" } })).toBeNull();
    expect(parseApiEvent({ event_type: "kb_updated", project_id: "p", payload: { status: "ready", doc: {} } })).toBeNull();
  });
});
