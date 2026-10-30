/*
 * Tamanho do navegador é temporário (spec 094, decisão 4), no shell montado: a geometria exibida
 * é derivada da salva e ajustada à área atual; nenhum PUT de layout sai por causa do resize. Uma
 * ação com a área reduzida grava só a janela-alvo, a partir do retângulo exibido.
 */
import { act, cleanup, fireEvent, within } from "@testing-library/react";
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
    // Gravado uma vez, na carga, com a versão (137/T02); nenhuma janela ia até a borda antiga.
    expect(server.model.ui.p1?.data.layouts).toEqual({ planning: { current: { v: 2, ...SAVED } } });
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

describe("layouts gravados com o dock (137/T02): migração única no shell", () => {
  // Gravados com o dock numa viewport de 1440×860: área de 732 px; as janelas de baixo iam até 720
  // (a folga de 12 dos presets). Sem o dock, a área é de 816 px.
  const LEGACY_CURRENT = {
    windows: {
      conversation: { x: 12, y: 12, w: 600, h: 708, z: 1, mode: "normal", open: true },
      plan: { x: 624, y: 12, w: 804, h: 400, z: 2, mode: "normal", open: true },
    },
  };
  const LEGACY_MINE = { windows: { plan: { x: 12, y: 300, w: 700, h: 420, z: 1, mode: "normal", open: true } } };

  it("o atual e o meu sem versão ganham a altura do dock embaixo, voltam ao servidor com a versão e o desfazer volta ao migrado", async () => {
    installWorkarea({ w: 1440, h: 816 });
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }) },
      ui: { p1: { rev: 3, data: { layouts: { planning: { current: LEGACY_CURRENT, mine: LEGACY_MINE } } } } },
    });
    await settle();
    // A Conversa ia até a borda antiga: desce até a nova (816 − 12). O Plano não ia: fica.
    expect(geometry("conversation")).toBe("12px,12px,600px,792px");
    expect(geometry("plan")).toBe("624px,12px,804px,400px");
    const layouts = server.model.ui.p1?.data.layouts as { planning: { current: unknown; mine: unknown } };
    expect(layouts.planning.current).toEqual({
      v: 2,
      windows: { conversation: { ...LEGACY_CURRENT.windows.conversation, h: 792 }, plan: LEGACY_CURRENT.windows.plan },
    });
    expect(layouts.planning.mine).toEqual({ v: 2, windows: { plan: { ...LEGACY_MINE.windows.plan, h: 504 } } });

    // O desfazer volta ao arranjo migrado, nunca ao antigo com o vão do dock.
    const conversation = document.querySelector<HTMLElement>('[data-window="conversation"]');
    if (conversation === null) throw new Error("conversa ausente");
    fireEvent.click(within(conversation).getByRole("button", { name: "Maximizar" }));
    expect(conversation.getAttribute("data-mode")).toBe("maximized");
    fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
    expect(conversation.getAttribute("data-mode")).toBe("normal");
    expect(geometry("conversation")).toBe("12px,12px,600px,792px");
  });
});
