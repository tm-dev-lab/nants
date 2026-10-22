/*
 * Tamanho do navegador é temporário (spec 094, decisão 4), no shell montado: a geometria exibida
 * é derivada da salva e ajustada à área atual; nenhum PUT de layout sai por causa do resize. Uma
 * ação com a área reduzida grava só a janela-alvo, a partir do retângulo exibido.
 */
import { act, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { makeProject, makeProjectState } from "../test-utils";
import { windowTitleBarId } from "./focus";
import { gestureSignal } from "./gestures";
import { renderShell, settle, type ServerCall } from "./shell-test-utils";
import { installPointerCapturePolyfill, manualScheduler, pointerEvent } from "./test-utils";

const P1 = makeProject("p1", "Loja");

afterEach(() => {
  cleanup();
  gestureSignal.reset();
  vi.restoreAllMocks();
});

const SAVED = {
  windows: {
    conversation: { x: 40, y: 40, w: 600, h: 700, z: 1, mode: "normal", open: true },
    plan: { x: 700, y: 60, w: 700, h: 700, z: 2, mode: "normal", open: true },
  },
};

/** Área de trabalho medida (o jsdom não tem layout): muda com `resizeTo`. */
function installWorkarea(initial: { w: number; h: number }) {
  let size = initial;
  const rect = (w: number, h: number): DOMRect =>
    ({ x: 0, y: 0, left: 0, top: 0, right: w, bottom: h, width: w, height: h, toJSON: () => ({}) }) as DOMRect;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this.hasAttribute("data-workarea") ? rect(size.w, size.h) : rect(0, 0);
  });
  return {
    async resizeTo(next: { w: number; h: number }): Promise<void> {
      size = next;
      await act(async () => {
        window.dispatchEvent(new Event("resize"));
      });
      await settle();
    },
  };
}

function geometry(kind: string): string {
  const el = document.querySelector<HTMLElement>(`[data-window="${kind}"]`);
  if (el === null) throw new Error(`janela ${kind} ausente`);
  return [el.style.left, el.style.top, el.style.width, el.style.height].join(",");
}

function layoutPuts(calls: readonly ServerCall[], from: number): unknown[] {
  return calls
    .slice(from)
    .filter((c) => c.method === "PUT" && /ui-state$/.test(c.url) && String((c.body as { section?: string }).section).startsWith("layouts"))
    .map((c) => c.body);
}

describe("tamanho do navegador temporário (shell)", () => {
  it("1440×900 → 900×700 → 1440×900: volta à mesma geometria e nenhum PUT de layout sai", async () => {
    const area = installWorkarea({ w: 1440, h: 760 });
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      ui: { p1: { rev: 3, data: { layouts: { planning: { current: SAVED } } } } },
    });
    await settle();
    const before = { conversation: geometry("conversation"), plan: geometry("plan") };
    expect(before.plan).toBe("700px,60px,700px,700px");
    const mark = server.calls.length;

    await area.resizeTo({ w: 900, h: 560 });
    // Exibida ajustada à área menor.
    expect(geometry("plan")).toBe("200px,0px,700px,560px");
    expect(geometry("conversation")).toBe("40px,0px,600px,560px");

    await area.resizeTo({ w: 1440, h: 760 });
    expect({ conversation: geometry("conversation"), plan: geometry("plan") }).toEqual(before);
    expect(layoutPuts(server.calls, mark)).toEqual([]);
    expect(server.model.ui.p1?.data.layouts).toEqual({ planning: { current: SAVED } });
  });

  it("1440 → 900, mover o Plano, voltar a 1440: a Conversa volta ao original e o Plano fica onde foi solto", async () => {
    const restoreCapture = installPointerCapturePolyfill();
    const scheduler = manualScheduler();
    try {
      const area = installWorkarea({ w: 1440, h: 760 });
      const { server } = await renderShell({
        url: "/?space=p1",
        projects: [P1],
        states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
        ui: { p1: { rev: 3, data: { layouts: { planning: { current: SAVED } } } } },
        scheduler,
      });
      await settle();
      const conversationBefore = geometry("conversation");
      await area.resizeTo({ w: 900, h: 560 });
      const bar = document.getElementById(windowTitleBarId("plan"));
      if (bar === null) throw new Error("barra ausente");
      // Arrasta 100 px para a esquerda (longe das bordas de encaixe).
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 400, clientY: 20 }));
        bar.dispatchEvent(pointerEvent("pointermove", { clientX: 300, clientY: 20 }));
        scheduler.runFrames();
      });
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerup", { clientX: 300, clientY: 20 }));
      });
      await settle();
      expect(geometry("plan")).toBe("100px,0px,700px,560px");

      await area.resizeTo({ w: 1440, h: 760 });
      expect(geometry("conversation")).toBe(conversationBefore);
      expect(geometry("plan")).toBe("100px,0px,700px,560px");
      await settle();
      // Gravado: só o Plano mudou; a Conversa continua com a geometria salva.
      const saved = (server.model.ui.p1?.data.layouts as { planning: { current: typeof SAVED } }).planning.current;
      expect(saved.windows.conversation).toEqual(SAVED.windows.conversation);
      expect(saved.windows.plan).toMatchObject({ x: 100, y: 0, w: 700, h: 560 });
    } finally {
      restoreCapture();
    }
  });
});
