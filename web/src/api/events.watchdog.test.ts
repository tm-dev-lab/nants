/*
 * Spec 080/T02: `ping`, `reconnectNow` e watchdog do EventStreamClient.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EventStreamClient, PING_EVENT, WATCHDOG_TIMEOUT_MS, type EventSourceLike } from "./events";

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
    const ev = new MessageEvent<string>(type, { data });
    for (const l of this.listeners.get(type) ?? []) l(ev);
  }
}

describe("EventStreamClient: ping, reconnectNow e watchdog", () => {
  let sources: FakeSource[];
  const factory = (): EventSourceLike => {
    const s = new FakeSource();
    sources.push(s);
    return s;
  };

  beforeEach(() => {
    vi.useFakeTimers();
    sources = [];
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("o prazo do watchdog é 2,5 × o ping de 15 s", () => {
    expect(WATCHDOG_TIMEOUT_MS).toBe(37_500);
  });

  it("ping não chega ao onEvent, mas conta como sinal de vida", () => {
    const onEvent = vi.fn();
    const onWatchdog = vi.fn();
    const client = new EventStreamClient({ onEvent, onWatchdog, createEventSource: factory, isVisible: () => true });
    client.start();
    for (let i = 0; i < 6; i += 1) {
      vi.advanceTimersByTime(15_000);
      sources[0]?.emit(PING_EVENT, "{}");
    }
    expect(onEvent).not.toHaveBeenCalled();
    expect(onWatchdog).not.toHaveBeenCalled();
    expect(sources).toHaveLength(1);
    expect(client.lastEventTime()).toBe(Date.now());
    client.close();
  });

  it("sem nenhum evento com a aba visível, reconecta e avisa para ressincronizar", () => {
    const onWatchdog = vi.fn();
    const client = new EventStreamClient({ onEvent: () => {}, onWatchdog, createEventSource: factory, isVisible: () => true });
    client.start();
    sources[0]?.onopen?.(new Event("open"));
    vi.advanceTimersByTime(35_000);
    expect(sources).toHaveLength(1);
    vi.advanceTimersByTime(5_000);
    expect(sources).toHaveLength(2);
    expect(sources[0]?.closed).toBe(true);
    expect(onWatchdog).toHaveBeenCalledTimes(1);
    client.close();
  });

  it("evento comum também adia o watchdog", () => {
    const onWatchdog = vi.fn();
    const client = new EventStreamClient({ onEvent: () => {}, onWatchdog, createEventSource: factory, isVisible: () => true });
    client.start();
    vi.advanceTimersByTime(30_000);
    sources[0]?.emit(
      "sdd_draft_updated",
      JSON.stringify({ event_type: "sdd_draft_updated", project_id: "p", payload: { project_id: "p" } }),
    );
    vi.advanceTimersByTime(30_000);
    expect(onWatchdog).not.toHaveBeenCalled();
    client.close();
  });

  it("aba oculta pausa o watchdog", () => {
    let visible = false;
    const onWatchdog = vi.fn();
    const client = new EventStreamClient({
      onEvent: () => {},
      onWatchdog,
      createEventSource: factory,
      isVisible: () => visible,
    });
    client.start();
    vi.advanceTimersByTime(300_000);
    expect(sources).toHaveLength(1);
    expect(onWatchdog).not.toHaveBeenCalled();
    // Ao voltar, o prazo recomeça (não dispara de imediato).
    visible = true;
    vi.advanceTimersByTime(30_000);
    expect(onWatchdog).not.toHaveBeenCalled();
    vi.advanceTimersByTime(15_000);
    expect(onWatchdog).toHaveBeenCalledTimes(1);
    client.close();
  });

  it("reconnectNow fecha a conexão atual, abre outra e cancela o backoff pendente", () => {
    const statuses: string[] = [];
    const client = new EventStreamClient({
      onEvent: () => {},
      onStatus: (s) => statuses.push(s),
      createEventSource: factory,
      isVisible: () => true,
    });
    client.start();
    sources[0]?.onerror?.(new Event("error"));
    client.reconnectNow();
    expect(sources).toHaveLength(2);
    vi.advanceTimersByTime(5_000);
    expect(sources).toHaveLength(2);
    client.reconnectNow();
    expect(sources[1]?.closed).toBe(true);
    expect(sources).toHaveLength(3);
    expect(statuses.at(-1)).toBe("connecting");
    client.close();
    client.reconnectNow();
    expect(sources).toHaveLength(3);
    vi.advanceTimersByTime(120_000);
    expect(sources).toHaveLength(3);
  });

  it("sem watchdog durante o backoff (não há conexão para vigiar)", () => {
    const onWatchdog = vi.fn();
    const client = new EventStreamClient({
      onEvent: () => {},
      onWatchdog,
      createEventSource: factory,
      isVisible: () => true,
      baseDelayMs: 60_000,
      maxDelayMs: 60_000,
    });
    client.start();
    sources[0]?.onerror?.(new Event("error"));
    vi.advanceTimersByTime(55_000);
    expect(onWatchdog).not.toHaveBeenCalled();
    client.close();
  });
});
