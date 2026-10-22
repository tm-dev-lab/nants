import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Desktop, useDesktopReducer } from "./Desktop";
import { BADGE_MAX, Dock, dockItemState, dockTipId } from "./Dock";
import { dockButtonId } from "./focus";
import { findForbiddenGlyphs } from "./glyphs";
import { fakeUiStateServer } from "./test-utils";
import { useUiState } from "./useUiState";
import { DOCK_ORDER } from "./windowMeta";
import type { Layout, WindowState } from "./windows";

afterEach(() => cleanup());

const win = (patch: Partial<WindowState> = {}): WindowState => ({
  x: 40,
  y: 40,
  w: 480,
  h: 360,
  z: 1,
  mode: "normal",
  open: true,
  ...patch,
});

const LAYOUT: Layout = {
  windows: {
    conversation: win({ z: 3 }),
    plan: win({ z: 2 }),
    board: win({ z: 1, mode: "minimized", restoreMode: "normal" }),
    activity: win({ open: false }),
  },
};

describe("Dock (componente)", () => {
  it("indicadores de aberta, minimizada e em foco; fechada sem indicador", () => {
    expect(dockItemState(LAYOUT, "conversation")).toBe("focused");
    expect(dockItemState(LAYOUT, "plan")).toBe("open");
    expect(dockItemState(LAYOUT, "board")).toBe("minimized");
    expect(dockItemState(LAYOUT, "activity")).toBe("closed");
    expect(dockItemState(LAYOUT, "files")).toBe("closed");
    render(<Dock kinds={["conversation", "plan", "board", "activity"]} layout={LAYOUT} onActivate={() => undefined} />);
    const indicator = (kind: string): string | null =>
      document.getElementById(`dock-${kind}`)?.querySelector("[data-indicator]")?.getAttribute("data-indicator") ?? null;
    expect(indicator("conversation")).toBe("focused");
    expect(indicator("plan")).toBe("open");
    expect(indicator("board")).toBe("minimized");
    expect(indicator("activity")).toBeNull();
  });

  it("ícones SVG, rótulo, Alt+1…9 e dica com o subtítulo (aria-describedby)", () => {
    render(
      <Dock
        kinds={["conversation", "plan"]}
        layout={LAYOUT}
        subtitles={{ plan: "4 tarefas · 2 concluídas" }}
        onActivate={() => undefined}
      />,
    );
    const nav = screen.getByRole("navigation", { name: "Dock" });
    const plan = within(nav).getByRole("button", { name: "Plano" });
    expect(plan.id).toBe(dockButtonId("plan"));
    expect(plan.querySelector("svg[data-icon='plan']")).not.toBeNull();
    expect(plan.getAttribute("aria-keyshortcuts")).toBe("Alt+2");
    expect(within(nav).getByRole("button", { name: "Conversa" }).getAttribute("aria-keyshortcuts")).toBe("Alt+1");
    expect(plan.getAttribute("aria-describedby")).toBe(dockTipId("plan"));
    const tip = document.getElementById(dockTipId("plan"));
    expect(tip?.getAttribute("role")).toBe("tooltip");
    expect(tip?.textContent).toBe("Plano · aberta · 4 tarefas · 2 concluídas");
  });

  it("selo por janela, com o texto acessível na dica e o número limitado", () => {
    render(
      <Dock
        kinds={["conversation", "plan"]}
        layout={LAYOUT}
        badges={{ conversation: { count: 2, label: "2 pendências" }, plan: { count: 140, label: "140 novas" } }}
        onActivate={() => undefined}
      />,
    );
    const badge = (kind: string): Element | null | undefined => document.getElementById(`dock-${kind}`)?.querySelector("[data-badge]");
    expect(badge("conversation")?.textContent).toBe("2");
    expect(badge("plan")?.textContent).toBe(String(BADGE_MAX));
    expect(document.getElementById(dockTipId("conversation"))?.textContent).toContain("2 pendências");
    expect(document.getElementById(dockTipId("plan"))?.textContent).toContain("140 novas");
    expect(findForbiddenGlyphs(document.body)).toEqual([]);
  });

  it("teclado: setas, Home e End percorrem os ícones; clique ativa", () => {
    const onActivate = vi.fn();
    render(<Dock kinds={["conversation", "plan", "board"]} layout={LAYOUT} onActivate={onActivate} />);
    const first = screen.getByRole("button", { name: "Conversa" });
    act(() => first.focus());
    fireEvent.keyDown(first, { key: "ArrowRight" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Plano" }));
    fireEvent.keyDown(document.activeElement ?? first, { key: "End" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Quadro" }));
    fireEvent.keyDown(document.activeElement ?? first, { key: "ArrowRight" });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: "ArrowLeft" });
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Quadro" }));
    fireEvent.click(screen.getByRole("button", { name: "Plano" }));
    expect(onActivate).toHaveBeenCalledWith("plan");
  });
});

function DesktopHarness({ layout }: { layout: Layout }) {
  const server = fakeUiStateServer();
  const globalState = useUiState({ kind: "global" }, { fetchImpl: server.fetchImpl, storage: null, createChannel: null });
  const [state, dispatch] = useDesktopReducer(layout, { w: 1200, h: 800 });
  return <Desktop state={state} dispatch={dispatch} globalState={globalState} />;
}

describe("Dock no Desktop", () => {
  it("ordem padrão do dock; Primeiros passos só quando aberta", () => {
    render(<DesktopHarness layout={{ windows: {} }} />);
    const names = within(screen.getByRole("navigation", { name: "Dock" }))
      .getAllByRole("button")
      .map((b) => b.id);
    expect(names).toEqual(DOCK_ORDER.filter((k) => k !== "setup").map(dockButtonId));
    cleanup();
    render(<DesktopHarness layout={{ windows: { setup: win() } }} />);
    expect(document.getElementById(dockButtonId("setup"))).not.toBeNull();
  });

  it("095/T03 (C-04): a Execução entra depois do App; Alt+1…9 cobre as nove primeiras; Projeto sem atalho", () => {
    expect(DOCK_ORDER).toEqual(["conversation", "plan", "board", "activity", "files", "app", "execution", "report", "costs", "project", "settings", "setup"]);
    render(<DesktopHarness layout={{ windows: {} }} />);
    const keys = (kind: string): string | null => document.getElementById(dockButtonId(kind as never))?.getAttribute("aria-keyshortcuts") ?? null;
    expect(keys("conversation")).toBe("Alt+1");
    expect(keys("app")).toBe("Alt+6");
    expect(keys("execution")).toBe("Alt+7");
    expect(keys("costs")).toBe("Alt+9");
    // O resto fica pelo lançador (sem Alt+0).
    expect(keys("project")).toBeNull();
    expect(keys("settings")).toBeNull();
    const execution = within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: "Execução" });
    expect(execution.querySelector("svg")).not.toBeNull();
  });

  it("clique: abre a fechada, foca a aberta, minimiza a que está em foco e restaura a minimizada", () => {
    render(<DesktopHarness layout={{ windows: { conversation: win({ z: 2 }), plan: win({ z: 1 }) } }} />);
    const dock = screen.getByRole("navigation", { name: "Dock" });
    const state = (kind: string): string | null => document.getElementById(`dock-${kind}`)?.getAttribute("data-state") ?? null;
    expect(state("conversation")).toBe("focused");
    fireEvent.click(within(dock).getByRole("button", { name: "Plano" }));
    expect(state("plan")).toBe("focused");
    fireEvent.click(within(dock).getByRole("button", { name: "Plano" }));
    expect(state("plan")).toBe("minimized");
    expect(document.querySelector<HTMLElement>("[data-window='plan']")?.hidden).toBe(true);
    fireEvent.click(within(dock).getByRole("button", { name: "Plano" }));
    expect(state("plan")).toBe("focused");
    expect(state("board")).toBe("closed");
    fireEvent.click(within(dock).getByRole("button", { name: "Quadro" }));
    expect(state("board")).toBe("focused");
    expect(screen.getByRole("region", { name: "Quadro" })).toBeTruthy();
  });

  it("minimizar anima até o ícone no dock e restaurar volta dele (transform)", () => {
    const animate = vi.fn();
    const proto = HTMLElement.prototype as HTMLElement & { animate?: unknown };
    const original = Object.getOwnPropertyDescriptor(proto, "animate");
    Object.defineProperty(proto, "animate", { configurable: true, writable: true, value: animate });
    const rects: Record<string, DOMRect> = {
      "dock-plan": new DOMRect(500, 850, 52, 52),
      plan: new DOMRect(100, 124, 480, 360),
    };
    const rectSpy = vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      return rects[this.id] ?? rects[this.getAttribute("data-window") ?? ""] ?? new DOMRect(0, 0, 0, 0);
    });
    try {
      render(<DesktopHarness layout={{ windows: { plan: win() } }} />);
      fireEvent.click(within(screen.getByRole("region", { name: "Plano" })).getByRole("button", { name: "Minimizar" }));
      const [out] = animate.mock.calls[0] as [Keyframe[]];
      expect(out[1]).toEqual({ transform: "translate(400px, 726px) scale(0.1083, 0.1444)", opacity: 0 });
      fireEvent.click(screen.getByRole("button", { name: "Plano" }));
      const [back] = animate.mock.calls[1] as [Keyframe[]];
      expect(back[0]).toEqual({ transform: "translate(400px, 726px) scale(0.1083, 0.1444)", opacity: 0 });
    } finally {
      rectSpy.mockRestore();
      if (original === undefined) delete (proto as { animate?: unknown }).animate;
      else Object.defineProperty(proto, "animate", original);
    }
  });

  it("Alt+1 restaura a janela minimizada da primeira posição", () => {
    render(<DesktopHarness layout={{ windows: { conversation: win({ mode: "minimized", restoreMode: "normal" }) } }} />);
    fireEvent.keyDown(document.body, { key: "1", code: "Digit1", altKey: true });
    expect(document.getElementById(dockButtonId("conversation"))?.getAttribute("data-state")).toBe("focused");
  });
});
