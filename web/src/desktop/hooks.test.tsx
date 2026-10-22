import { act, cleanup, render, renderHook, screen } from "@testing-library/react";
import { useRef, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { gestureSignal } from "./gestures";
import { fakeResizeObservers, fakeUiStateServer, installPointerCapturePolyfill, manualScheduler, memoryStorage, pointerEvent } from "./test-utils";
import { parseSelection, UI_SECTIONS, type UiScope } from "./uiState";
import { useContainerSize, sizeForWidth } from "./useContainerSize";
import { useUiSection, useUiState } from "./useUiState";
import { useGestureActive, useWindowGesture, type GestureCommit } from "./useWindowGesture";
import { isWindowVisible, useWindowVisibility, WindowLayoutContext } from "./useWindowVisibility";
import { parseLayout, type Layout, type Rect, type WindowState } from "./windows";

afterEach(() => cleanup());

async function settle(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 20; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

describe("useUiState", () => {
  it("lê o servidor, grava pela seção e manda keepalive no pagehide", async () => {
    const server = fakeUiStateServer({ rev: 2, data: { selection: { taskId: "T1" } } });
    const deps = { fetchImpl: server.fetchImpl, storage: memoryStorage(), createChannel: null };
    const { result } = renderHook(() => {
      const handle = useUiState({ kind: "project", projectId: "p1" }, deps);
      const selection = useUiSection(handle, UI_SECTIONS.selection, parseSelection);
      return { handle, selection };
    });
    await settle();
    expect(result.current.handle.snapshot).toMatchObject({ rev: 2, ready: true });
    expect(result.current.selection.taskId).toBe("T1");
    act(() => {
      result.current.handle.set(UI_SECTIONS.selection, { taskId: "T2", filePath: "a.ts" });
    });
    expect(result.current.selection).toMatchObject({ taskId: "T2", filePath: "a.ts" });
    act(() => {
      window.dispatchEvent(new Event("pagehide"));
    });
    await settle();
    expect(server.puts()).toEqual([
      expect.objectContaining({ keepalive: true, body: { base_rev: 2, section: "selection", value: { taskId: "T2", filePath: "a.ts" } } }),
    ]);
  });

  it("trocar de espaço manda o que estava pendente e lê o novo escopo", async () => {
    const server = fakeUiStateServer({ rev: 0 });
    const deps = { fetchImpl: server.fetchImpl, storage: null, createChannel: null };
    const { result, rerender } = renderHook(({ scope }: { scope: UiScope }) => useUiState(scope, deps), {
      initialProps: { scope: { kind: "project", projectId: "p1" } as UiScope },
    });
    await settle();
    act(() => {
      result.current.set(UI_SECTIONS.mobile, { tab: "plan" });
    });
    rerender({ scope: { kind: "project", projectId: "p2" } });
    await settle();
    const urls = server.calls.map((c) => `${c.method} ${c.url}`);
    expect(urls).toContain("PUT /api/projects/p1/ui-state");
    expect(urls).toContain("GET /api/projects/p2/ui-state");
  });
});

describe("useUiState: identidade estável", () => {
  it("o handle e o valor parseado mantêm a identidade; outra seção não recria o valor", async () => {
    const layout = { windows: { plan: { x: 0, y: 0, w: 400, h: 300, z: 1, mode: "normal", open: true } } };
    const server = fakeUiStateServer({ rev: 1, data: { layouts: { execution: { current: layout } } } });
    const timers: (() => void)[] = [];
    const deps = {
      fetchImpl: server.fetchImpl,
      storage: null,
      createChannel: null,
      setTimer: (fn: () => void) => timers.push(fn),
      clearTimer: () => undefined,
    };
    const { result, rerender } = renderHook(() => {
      const handle = useUiState({ kind: "project", projectId: "p1" }, deps);
      const current = useUiSection(handle, UI_SECTIONS.layout("execution", "current"), parseLayout);
      return { handle, current };
    });
    await settle();
    const first = result.current;
    expect(first.current?.windows.plan?.w).toBe(400);
    rerender();
    expect(result.current.handle).toBe(first.handle);
    expect(result.current.current).toBe(first.current);
    act(() => {
      result.current.handle.set(UI_SECTIONS.selection, { taskId: "T2" });
    });
    expect(result.current.current).toBe(first.current);
    // Depois do PUT (envelope novo do servidor), a seção intacta continua a mesma.
    await act(async () => {
      timers.shift()?.();
    });
    await settle();
    expect(server.puts()).toHaveLength(1);
    expect(result.current.current).toBe(first.current);
  });

  it("aba oculta descarrega pelo envio sequencial (também com keepalive)", async () => {
    const server = fakeUiStateServer({ rev: 0 });
    const deps = { fetchImpl: server.fetchImpl, storage: null, createChannel: null, setTimer: () => 0, clearTimer: () => undefined };
    const { result } = renderHook(() => useUiState({ kind: "project", projectId: "p1" }, deps));
    await settle();
    act(() => {
      result.current.set(UI_SECTIONS.mobile, { tab: "plan" });
    });
    const state = vi.spyOn(document, "visibilityState", "get").mockReturnValue("hidden");
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await settle();
    state.mockRestore();
    expect(server.puts()).toEqual([expect.objectContaining({ keepalive: true, body: { base_rev: 0, section: "mobile", value: { tab: "plan" } } })]);
  });
});

describe("useWindowGesture", () => {
  let restorePolyfill: () => void = () => undefined;
  beforeEach(() => {
    restorePolyfill = installPointerCapturePolyfill();
    gestureSignal.reset();
  });
  afterEach(() => restorePolyfill());

  function Harness(props: { rect: Rect; onCommit: (c: GestureCommit) => void; scheduler: ReturnType<typeof manualScheduler>; enabled?: boolean }) {
    const ref = useRef<HTMLDivElement>(null);
    const gesture = useWindowGesture({
      windowRef: ref,
      rect: props.rect,
      workspace: { w: 1000, h: 700 },
      onCommit: props.onCommit,
      scheduler: props.scheduler,
      enabled: props.enabled,
    });
    const iframeOff = useGestureActive();
    return (
      <div ref={ref} data-testid="win">
        <div data-testid="bar" onPointerDown={gesture.onMovePointerDown}>
          <button type="button">Fechar</button>
        </div>
        <div data-testid="se" onPointerDown={gesture.onResizePointerDown("resize-se")} />
        <span data-testid="snap">{gesture.snapPreview ?? "nenhum"}</span>
        <span data-testid="iframes">{iframeOff ? "off" : "on"}</span>
      </div>
    );
  }

  const rect: Rect = { x: 100, y: 100, w: 400, h: 300 };

  it("mover: transform por rAF durante o gesto e commit só no pointerup", () => {
    const onCommit = vi.fn();
    const scheduler = manualScheduler();
    render(<Harness rect={rect} onCommit={onCommit} scheduler={scheduler} />);
    const bar = screen.getByTestId("bar");
    const win = screen.getByTestId("win");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 200, clientY: 120 }));
    });
    expect(bar.hasPointerCapture(1)).toBe(true);
    expect(screen.getByTestId("iframes").textContent).toBe("off");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointermove", { clientX: 250, clientY: 160 }));
      bar.dispatchEvent(pointerEvent("pointermove", { clientX: 260, clientY: 170 }));
    });
    // Um só quadro agendado para os dois movimentos; nada aplicado antes do rAF.
    expect(scheduler.pending()).toBe(1);
    expect(win.style.transform).toBe("");
    act(() => {
      scheduler.runFrames();
    });
    expect(win.style.transform).toBe("translate(60px, 50px)");
    expect(onCommit).not.toHaveBeenCalled();
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerup", { clientX: 260, clientY: 170 }));
    });
    expect(onCommit).toHaveBeenCalledWith({ rect: { x: 160, y: 150, w: 400, h: 300 }, snap: null });
    expect(win.style.transform).toBe("");
    expect(screen.getByTestId("iframes").textContent).toBe("on");
  });

  it("prévia de encaixe a 16 px da borda e encaixe no commit", () => {
    const onCommit = vi.fn();
    render(<Harness rect={rect} onCommit={onCommit} scheduler={manualScheduler()} />);
    const bar = screen.getByTestId("bar");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 200, clientY: 120 }));
      bar.dispatchEvent(pointerEvent("pointermove", { clientX: 10, clientY: 300 }));
    });
    expect(screen.getByTestId("snap").textContent).toBe("left");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointermove", { clientX: 5, clientY: 5 }));
    });
    expect(screen.getByTestId("snap").textContent).toBe("top-left");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointermove", { clientX: 500, clientY: 4 }));
    });
    expect(screen.getByTestId("snap").textContent).toBe("top");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerup", { clientX: 500, clientY: 4 }));
    });
    expect(onCommit.mock.calls[0]?.[0]).toMatchObject({ snap: "top" });
    expect(screen.getByTestId("snap").textContent).toBe("nenhum");
  });

  it("redimensionar pelo canto respeita o mínimo; pointercancel não grava", () => {
    const onCommit = vi.fn();
    const scheduler = manualScheduler();
    render(<Harness rect={rect} onCommit={onCommit} scheduler={scheduler} />);
    const se = screen.getByTestId("se");
    const win = screen.getByTestId("win");
    act(() => {
      se.dispatchEvent(pointerEvent("pointerdown", { clientX: 500, clientY: 400 }));
      se.dispatchEvent(pointerEvent("pointermove", { clientX: 100, clientY: 100 }));
      scheduler.runFrames();
    });
    expect(win.style.width).toBe("320px");
    expect(win.style.height).toBe("220px");
    act(() => {
      se.dispatchEvent(pointerEvent("pointercancel"));
    });
    expect(onCommit).not.toHaveBeenCalled();
    expect(win.style.width).toBe("");
    expect(gestureSignal.active()).toBe(false);
  });

  it("botões da barra, botão secundário e gesto desligado não arrastam; clique sem movimento não grava", () => {
    const onCommit = vi.fn();
    const { rerender } = render(<Harness rect={rect} onCommit={onCommit} scheduler={manualScheduler()} />);
    act(() => {
      screen.getByRole("button", { name: "Fechar" }).dispatchEvent(pointerEvent("pointerdown", { clientX: 1, clientY: 1 }));
    });
    expect(gestureSignal.active()).toBe(false);
    const bar = screen.getByTestId("bar");
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerdown", { button: 2 }));
    });
    expect(gestureSignal.active()).toBe(false);
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 10, clientY: 10 }));
      bar.dispatchEvent(pointerEvent("pointerup", { clientX: 10, clientY: 10 }));
    });
    expect(onCommit).not.toHaveBeenCalled();
    rerender(<Harness rect={rect} onCommit={onCommit} scheduler={manualScheduler()} enabled={false} />);
    act(() => {
      bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 10, clientY: 10 }));
    });
    expect(gestureSignal.active()).toBe(false);
  });

  it("desmontar no meio do gesto libera o sinal dos iframes", () => {
    const { unmount } = render(<Harness rect={rect} onCommit={vi.fn()} scheduler={manualScheduler()} />);
    act(() => {
      screen.getByTestId("bar").dispatchEvent(pointerEvent("pointerdown", { clientX: 10, clientY: 10 }));
    });
    expect(gestureSignal.active()).toBe(true);
    unmount();
    expect(gestureSignal.active()).toBe(false);
  });
});

describe("useWindowVisibility", () => {
  const w = (patch: Partial<WindowState> = {}): WindowState => ({ x: 0, y: 0, w: 400, h: 300, z: 1, mode: "normal", open: true, ...patch });
  const layout: Layout = {
    windows: {
      conversation: w({ z: 1 }),
      plan: w({ z: 2, mode: "maximized" }),
      files: w({ z: 3 }),
      board: w({ mode: "minimized" }),
      app: w({ open: false }),
    },
  };

  it("regra pura: minimizada, fechada, coberta por maximizada e aba oculta", () => {
    expect(isWindowVisible(layout, "plan", true)).toBe(true);
    expect(isWindowVisible(layout, "files", true)).toBe(true);
    expect(isWindowVisible(layout, "conversation", true)).toBe(false);
    expect(isWindowVisible(layout, "board", true)).toBe(false);
    expect(isWindowVisible(layout, "app", true)).toBe(false);
    expect(isWindowVisible(layout, "activity", true)).toBe(false);
    expect(isWindowVisible(layout, "plan", false)).toBe(false);
    expect(isWindowVisible(null, "plan", true)).toBe(false);
  });

  it("hook reage à aba oculta", () => {
    const wrapper = ({ children }: { children: ReactNode }) => (
      <WindowLayoutContext.Provider value={layout}>{children}</WindowLayoutContext.Provider>
    );
    const { result } = renderHook(() => useWindowVisibility("plan"), { wrapper });
    expect(result.current).toBe(true);
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current).toBe(false);
    hidden.mockReturnValue(false);
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(result.current).toBe(true);
  });
});

describe("useContainerSize", () => {
  it("faixas de largura", () => {
    expect(sizeForWidth(300)).toBe("narrow");
    expect(sizeForWidth(479)).toBe("narrow");
    expect(sizeForWidth(480)).toBe("medium");
    expect(sizeForWidth(839)).toBe("medium");
    expect(sizeForWidth(840)).toBe("wide");
  });

  it("observador injetável define data-size e desliga ao desmontar", () => {
    const observers = fakeResizeObservers();
    function Box() {
      const ref = useRef<HTMLDivElement>(null);
      const size = useContainerSize(ref, { createObserver: observers.factory });
      return (
        <div ref={ref} data-testid="box">
          {size}
        </div>
      );
    }
    const { unmount } = render(<Box />);
    const box = screen.getByTestId("box");
    expect(box.dataset.size).toBe("wide");
    act(() => observers.resize(300));
    expect(box.dataset.size).toBe("narrow");
    expect(box.textContent).toBe("narrow");
    act(() => observers.resize(600));
    expect(box.dataset.size).toBe("medium");
    expect(observers.active()).toBe(1);
    unmount();
    expect(observers.active()).toBe(0);
  });

  it("sem ResizeObserver não quebra", () => {
    function Box() {
      const ref = useRef<HTMLDivElement>(null);
      useContainerSize(ref, { createObserver: null, initial: "medium" });
      return <div ref={ref} data-testid="box2" />;
    }
    render(<Box />);
    expect(screen.getByTestId("box2").dataset.size).toBe("medium");
  });
});
