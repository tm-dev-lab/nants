import { afterEach, describe, expect, it, vi } from "vitest";
import { installNantyPresence, PRESENCE_PATH } from "./nantyPresence";

function setVisibility(state: DocumentVisibilityState): void {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
  document.dispatchEvent(new Event("visibilitychange"));
}

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
});

describe("installNantyPresence", () => {
  it("avisa visível ao montar, renova enquanto visível e avisa oculta com keepalive", () => {
    vi.useFakeTimers();
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
    const calls: [string, boolean, boolean][] = [];
    const off = installNantyPresence({ send: (tab, visible, keepalive) => calls.push([tab, visible, keepalive]), heartbeatMs: 1000, tabId: "aba-1" });
    expect(calls).toEqual([["aba-1", true, false]]);
    vi.advanceTimersByTime(2100);
    expect(calls.length).toBe(3);
    setVisibility("hidden");
    expect(calls.at(-1)).toEqual(["aba-1", false, true]);
    vi.advanceTimersByTime(5000);
    expect(calls.length).toBe(4);
    setVisibility("visible");
    expect(calls.at(-1)).toEqual(["aba-1", true, false]);
    window.dispatchEvent(new Event("pagehide"));
    expect(calls.at(-1)).toEqual(["aba-1", false, true]);
    off();
    expect(calls.at(-1)).toEqual(["aba-1", false, true]);
    const count = calls.length;
    setVisibility("visible");
    vi.advanceTimersByTime(5000);
    expect(calls.length).toBe(count);
  });

  it("manda o sinal leve para o servidor, com um id de aba aleatório", async () => {
    const fetchMock = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
    const off = installNantyPresence();
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const [path, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(path).toBe(PRESENCE_PATH);
    expect(init.method).toBe("POST");
    const body = JSON.parse(String(init.body)) as { tab: string; visible: boolean };
    expect(body.visible).toBe(true);
    expect(body.tab).toMatch(/^[0-9a-f]{16}$/);
    off();
  });
});
