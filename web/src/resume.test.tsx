/*
 * Spec 080/T02: retomada da aba (visibilitychange, pageshow, online) reabre o stream global e
 * ressincroniza uma vez só (debounce); `ping` não altera o store.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { createResumeScheduler, installResumeSync, useLiveStream, RESUME_DEBOUNCE_MS } from "./liveStream";
import { appStore } from "./store";
import {
  FakeEventSource,
  installFakeEventSource,
  installFetch,
  makeConfig,
  makeProject,
  makeProjectState,
  resetStore,
} from "./test-utils";

/** Assinatura global montada como no desktop (`useDesktopRuntime`). */
function GlobalStream(): null {
  useLiveStream();
  return null;
}

/** Documento mínimo com `visibilityState` controlável. */
class FakeDoc extends EventTarget {
  visibilityState: DocumentVisibilityState = "visible";
  set(state: DocumentVisibilityState): void {
    this.visibilityState = state;
    this.dispatchEvent(new Event("visibilitychange"));
  }
}

function pageshow(persisted: boolean): Event {
  const ev = new Event("pageshow");
  Object.defineProperty(ev, "persisted", { value: persisted });
  return ev;
}

describe("installResumeSync + createResumeScheduler", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  function setup() {
    const reconnect = vi.fn();
    const resync = vi.fn();
    const doc = new FakeDoc();
    const win = new EventTarget();
    let clock = 0;
    const scheduler = createResumeScheduler(reconnect, resync);
    const uninstall = installResumeSync({
      scheduler,
      doc: doc as unknown as Document,
      win: win as unknown as Window,
      now: () => clock,
    });
    return {
      reconnect,
      resync,
      doc,
      win,
      advance: (ms: number) => {
        clock += ms;
      },
      scheduler,
      uninstall,
    };
  }

  it("visível depois de 2 s oculta reconecta e ressincroniza", () => {
    const t = setup();
    t.doc.set("hidden");
    t.advance(2_000);
    t.doc.set("visible");
    expect(t.reconnect).not.toHaveBeenCalled();
    vi.advanceTimersByTime(RESUME_DEBOUNCE_MS);
    expect(t.reconnect).toHaveBeenCalledTimes(1);
    expect(t.resync).toHaveBeenCalledTimes(1);
    t.uninstall();
  });

  it("oculta por menos de 2 s não dispara", () => {
    const t = setup();
    t.doc.set("hidden");
    t.advance(1_999);
    t.doc.set("visible");
    vi.advanceTimersByTime(5_000);
    expect(t.reconnect).not.toHaveBeenCalled();
    expect(t.resync).not.toHaveBeenCalled();
    t.uninstall();
  });

  it("pageshow só com persisted; online sempre", () => {
    const t = setup();
    t.win.dispatchEvent(pageshow(false));
    vi.advanceTimersByTime(5_000);
    expect(t.resync).not.toHaveBeenCalled();
    t.win.dispatchEvent(pageshow(true));
    vi.advanceTimersByTime(RESUME_DEBOUNCE_MS);
    expect(t.reconnect).toHaveBeenCalledTimes(1);
    t.win.dispatchEvent(new Event("online"));
    vi.advanceTimersByTime(RESUME_DEBOUNCE_MS);
    expect(t.reconnect).toHaveBeenCalledTimes(2);
    expect(t.resync).toHaveBeenCalledTimes(2);
    t.uninstall();
  });

  it("disparos em sequência viram uma reconexão e uma ressincronização", () => {
    const t = setup();
    t.doc.set("hidden");
    t.advance(10_000);
    t.doc.set("visible");
    t.win.dispatchEvent(pageshow(true));
    t.win.dispatchEvent(new Event("online"));
    t.scheduler.schedule(false); // watchdog no meio da rajada
    vi.advanceTimersByTime(RESUME_DEBOUNCE_MS * 3);
    expect(t.reconnect).toHaveBeenCalledTimes(1);
    expect(t.resync).toHaveBeenCalledTimes(1);
    t.uninstall();
  });

  it("watchdog sozinho ressincroniza sem reconectar de novo", () => {
    const t = setup();
    t.scheduler.schedule(false);
    vi.advanceTimersByTime(RESUME_DEBOUNCE_MS);
    expect(t.reconnect).not.toHaveBeenCalled();
    expect(t.resync).toHaveBeenCalledTimes(1);
    t.uninstall();
  });

  it("desinstalado, não reage mais", () => {
    const t = setup();
    t.uninstall();
    t.scheduler.dispose();
    t.win.dispatchEvent(new Event("online"));
    vi.advanceTimersByTime(5_000);
    expect(t.resync).not.toHaveBeenCalled();
  });
});

describe("useLiveStream: retomada reabre o stream e ressincroniza", () => {
  beforeEach(() => {
    installFakeEventSource();
    resetStore({ config: makeConfig([makeProject("p1", "api")]), activeProjectId: "p1" });
  });
  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  function server() {
    return installFetch((_method, url) => {
      if (url === "/api/projects/p1/state") return { body: makeProjectState({ id: "p1", name: "api" }) };
      if (url === "/api/projects/p1/chat") return { body: { turns: [] } };
      if (url === "/api/projects/p1/activity") return { body: [] };
      return { status: 404, body: { error: "não encontrado", code: "not_found" } };
    });
  }

  const callsTo = (mock: ReturnType<typeof installFetch>, path: string): number =>
    mock.mock.calls.filter((c) => String(c[0]) === path).length;

  it("pageshow persisted + online em rajada: uma reconexão, uma ressincronização completa", async () => {
    const fetchMock = server();
    render(<GlobalStream />);
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    const revisionBefore = appStore.getState().revisions.p1 ?? 0;

    act(() => {
      window.dispatchEvent(pageshow(true));
      window.dispatchEvent(new Event("online"));
      window.dispatchEvent(pageshow(true));
    });

    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(2));
    expect(FakeEventSource.instances[0]?.readyState).toBe(2);
    await waitFor(() => expect(callsTo(fetchMock, "/api/projects/p1/chat")).toBe(1));
    expect(callsTo(fetchMock, "/api/projects/p1/activity")).toBe(1);
    expect(appStore.getState().revisions.p1).toBe(revisionBefore + 1);
    // Nada mais chega depois da janela de debounce.
    await act(async () => {
      await new Promise((r) => setTimeout(r, RESUME_DEBOUNCE_MS * 2));
    });
    expect(FakeEventSource.instances).toHaveLength(2);
    expect(callsTo(fetchMock, "/api/projects/p1/chat")).toBe(1);
  });

  it("visibilitychange depois de 2 s oculta reabre o stream e ressincroniza", async () => {
    const fetchMock = server();
    let state: DocumentVisibilityState = "visible";
    vi.spyOn(document, "visibilityState", "get").mockImplementation(() => state);
    render(<GlobalStream />);
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));

    // A retomada mede o tempo oculta pelo relógio monotônico (114/T01), não pelo de parede.
    let clock = performance.now();
    const nowSpy = vi.spyOn(performance, "now").mockImplementation(() => clock);
    state = "hidden";
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    clock += 5_000;
    state = "visible";
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    nowSpy.mockRestore();

    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(2));
    await waitFor(() => expect(callsTo(fetchMock, "/api/projects/p1/chat")).toBe(1));
  });

  it("ping não altera o store", async () => {
    server();
    render(<GlobalStream />);
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    const before = appStore.getState();
    act(() => FakeEventSource.instances[0]?.emit("ping", {}));
    expect(appStore.getState()).toBe(before);
  });
});
