/*
 * Modo celular (spec 093, T10; spec 082/T06): o teclado virtual não cobre a caixa de mensagem
 * (meta viewport, --app-height e foco), com o limite do celular em 1024 px e os componentes da janela Conversa.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  APP_HEIGHT_VAR,
  KEYBOARD_RESIZE_DEBOUNCE_MS,
  KEYBOARD_SETTLE_MS,
  MOBILE_QUERY,
  installAppHeight,
  installKeyboardReveal,
  useAppHeight,
  useKeyboardReveal,
} from "./viewport";
import { AutoScrollBox } from "../apps/conversation/AutoScrollBox";
import { ChoicesForm } from "../apps/conversation/ChoicesForm";

/** `visualViewport` falso: altura/escala mutáveis e listeners observáveis. */
class FakeVisualViewport extends EventTarget {
  height = 800;
  scale = 1;
  readonly added: string[] = [];
  readonly removed: string[] = [];
  override addEventListener(type: string, listener: EventListenerOrEventListenerObject | null): void {
    this.added.push(type);
    super.addEventListener(type, listener);
  }
  override removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null): void {
    this.removed.push(type);
    super.removeEventListener(type, listener);
  }
}

let vv: FakeVisualViewport | null = null;

function setVisualViewport(value: FakeVisualViewport | null): void {
  Object.defineProperty(window, "visualViewport", { configurable: true, writable: true, value });
}

function setMobile(mobile: boolean): void {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: query === MOBILE_QUERY ? mobile : false,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

/** rAF síncrono e contável (uma atualização por frame). */
let frames: FrameRequestCallback[] = [];
function flushFrames(): void {
  const pending = frames;
  frames = [];
  pending.forEach((cb) => cb(0));
}

/** Timers falsos sem tocar no rAF (o rAF é o espião contável acima). */
function useFakeTimers(): void {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"] });
}

const appHeight = (): string => document.documentElement.style.getPropertyValue(APP_HEIGHT_VAR);

beforeEach(() => {
  vv = new FakeVisualViewport();
  setVisualViewport(vv);
  frames = [];
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
    frames.push(cb);
    return frames.length;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => undefined);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  document.documentElement.style.removeProperty(APP_HEIGHT_VAR);
  Reflect.deleteProperty(window, "visualViewport");
  Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});

describe("meta viewport (index.html)", () => {
  it("layout encolhe com o teclado e mantém o resto", () => {
    const html = readFileSync(resolve(process.cwd(), "index.html"), "utf8");
    const meta = /<meta name="viewport" content="([^"]+)"/.exec(html)?.[1] ?? "";
    const parts = meta.split(",").map((p) => p.trim());
    expect(parts).toEqual([
      "width=device-width",
      "initial-scale=1.0",
      "viewport-fit=cover",
      "interactive-widget=resizes-content",
    ]);
  });
});

describe("--app-height (visualViewport)", () => {
  it("grava a altura visível e atualiza no resize, uma vez por frame", () => {
    const remove = installAppHeight();
    expect(appHeight()).toBe("800px");

    if (vv === null) throw new Error("vv");
    vv.height = 480;
    vv.dispatchEvent(new Event("resize"));
    vv.dispatchEvent(new Event("scroll"));
    vv.dispatchEvent(new Event("resize"));
    expect(frames).toHaveLength(1);
    expect(appHeight()).toBe("800px");
    flushFrames();
    expect(appHeight()).toBe("480px");

    remove();
  });

  it("com zoom de pinça usa a altura em px de layout", () => {
    if (vv === null) throw new Error("vv");
    vv.height = 400;
    vv.scale = 2;
    const remove = installAppHeight();
    expect(appHeight()).toBe("800px");
    remove();
  });

  it("sem visualViewport não lança e não define a variável", () => {
    setVisualViewport(null);
    expect(() => installAppHeight()()).not.toThrow();
    expect(appHeight()).toBe("");
  });

  it("hook remove os listeners e a variável ao desmontar", () => {
    function Shell() {
      useAppHeight();
      return null;
    }
    const view = render(<Shell />);
    expect(appHeight()).toBe("800px");
    expect(vv?.added).toEqual(expect.arrayContaining(["resize", "scroll"]));

    view.unmount();

    expect(vv?.removed).toEqual(expect.arrayContaining(["resize", "scroll"]));
    if (vv === null) throw new Error("vv");
    vv.height = 300;
    vv.dispatchEvent(new Event("resize"));
    expect(frames).toHaveLength(0);
    expect(appHeight()).toBe("");
  });
});

function Composer() {
  const ref = useKeyboardReveal();
  return (
    <div ref={ref} data-testid="dock">
      <label>
        Mensagem
        <textarea />
      </label>
      <button type="submit">Enviar</button>
    </div>
  );
}

describe("foco na caixa de mensagem", () => {
  function spyScroll() {
    const calls: { el: Element; arg: unknown }[] = [];
    Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
      configurable: true,
      writable: true,
      value: function (this: Element, arg: unknown) {
        calls.push({ el: this, arg });
      },
    });
    return calls;
  }

  /** Avança os timers falsos e roda os quadros pendentes (rAF da revelação). */
  function advance(ms: number): void {
    act(() => {
      vi.advanceTimersByTime(ms);
      flushFrames();
    });
  }

  function resize(height: number): void {
    act(() => {
      if (vv === null) throw new Error("vv");
      vv.height = height;
      vv.dispatchEvent(new Event("resize"));
    });
  }

  it("celular: teclado subindo em degraus revela uma única vez, depois do último degrau (nearest)", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(<Composer />);

    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem" }));
    expect(calls).toHaveLength(0);
    for (const height of [760, 700, 640, 580, 520, 470]) {
      resize(height);
      advance(40);
      expect(calls).toHaveLength(0);
    }
    // Já passaram 240 ms desde o foco; o fallback de 300 ms não dispara no meio dos degraus.
    advance(KEYBOARD_RESIZE_DEBOUNCE_MS - 40 - 1);
    expect(calls).toHaveLength(0);
    // O timer vence, mas a revelação espera o quadro (depois do --app-height).
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(calls).toHaveLength(0);
    act(() => flushFrames());

    expect(calls).toEqual([{ el: screen.getByTestId("dock"), arg: { block: "nearest" } }]);
    advance(KEYBOARD_SETTLE_MS * 3);
    expect(calls).toHaveLength(1);
  });

  it("resize posterior com o campo ainda focado revela de novo (com debounce)", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(<Composer />);

    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem" }));
    resize(470);
    advance(KEYBOARD_RESIZE_DEBOUNCE_MS);
    expect(calls).toHaveLength(1);

    // Troca de teclado: dois degraus próximos, uma revelação.
    resize(430);
    advance(40);
    resize(420);
    advance(KEYBOARD_RESIZE_DEBOUNCE_MS - 1);
    expect(calls).toHaveLength(1);
    advance(1);
    expect(calls).toHaveLength(2);
  });

  it("depois do focusout para fora do contêiner, resize não revela", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(<Composer />);
    const field = screen.getByRole("textbox", { name: "Mensagem" });

    fireEvent.focusIn(field);
    resize(470);
    fireEvent.focusOut(field, { relatedTarget: document.body });
    advance(KEYBOARD_SETTLE_MS * 2);
    resize(800);
    advance(KEYBOARD_SETTLE_MS * 2);

    expect(calls).toHaveLength(0);
    expect(vv?.removed).toContain("resize");
  });

  it("focusout para dentro do contêiner não cancela; novo focusin sem teclado limpa", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(<Composer />);
    const field = screen.getByRole("textbox", { name: "Mensagem" });
    const send = screen.getByRole("button", { name: "Enviar" });

    fireEvent.focusIn(field);
    fireEvent.focusOut(field, { relatedTarget: send });
    resize(470);
    advance(KEYBOARD_RESIZE_DEBOUNCE_MS);
    expect(calls).toHaveLength(1);

    fireEvent.focusIn(send);
    resize(800);
    advance(KEYBOARD_SETTLE_MS * 2);
    expect(calls).toHaveLength(1);
  });

  it("sem resize, revela após o tempo de assentamento do teclado", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(<Composer />);

    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem" }));
    advance(KEYBOARD_SETTLE_MS - 1);
    expect(calls).toHaveLength(0);
    advance(1);
    expect(calls).toHaveLength(1);
    advance(KEYBOARD_SETTLE_MS * 3);
    expect(calls).toHaveLength(1);
  });

  it("desktop: nenhum efeito", () => {
    useFakeTimers();
    setMobile(false);
    const calls = spyScroll();
    render(<Composer />);

    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem" }));
    act(() => {
      vv?.dispatchEvent(new Event("resize"));
      vi.advanceTimersByTime(KEYBOARD_SETTLE_MS * 2);
    });
    expect(calls).toHaveLength(0);
  });

  it("foco em botão não abre teclado: nada acontece", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(<Composer />);

    fireEvent.focusIn(screen.getByRole("button", { name: "Enviar" }));
    act(() => {
      vi.advanceTimersByTime(KEYBOARD_SETTLE_MS * 2);
    });
    expect(calls).toHaveLength(0);
  });

  it("remoção desfaz focusin, focusout, timer, quadro e listener de resize", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    const clearSpy = vi.spyOn(globalThis, "clearTimeout");
    const dock = document.createElement("div");
    const area = document.createElement("textarea");
    dock.append(area);
    document.body.append(dock);
    const removeListener = vi.spyOn(dock, "removeEventListener");
    const remove = installKeyboardReveal(dock);

    fireEvent.focusIn(area);
    resize(470);
    expect(vi.getTimerCount()).toBe(1);
    remove();

    expect(vi.getTimerCount()).toBe(0);
    expect(clearSpy).toHaveBeenCalled();
    expect(removeListener.mock.calls.map((c) => c[0])).toEqual(expect.arrayContaining(["focusin", "focusout"]));
    expect(vv?.removed).toContain("resize");
    resize(400);
    advance(KEYBOARD_SETTLE_MS * 2);
    fireEvent.focusIn(area);
    advance(KEYBOARD_SETTLE_MS * 2);

    expect(vi.getTimerCount()).toBe(0);
    expect(frames).toHaveLength(0);
    expect(calls).toHaveLength(0);
    dock.remove();
  });

  it("remoção com a revelação esperando o quadro cancela o quadro", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    const dock = document.createElement("div");
    const area = document.createElement("textarea");
    dock.append(area);
    document.body.append(dock);
    const remove = installKeyboardReveal(dock);

    fireEvent.focusIn(area);
    act(() => {
      vi.advanceTimersByTime(KEYBOARD_SETTLE_MS);
    });
    expect(frames).toHaveLength(1);
    remove();
    expect(window.cancelAnimationFrame).toHaveBeenCalledWith(1);
    dock.remove();
    expect(calls).toHaveLength(0);
  });

  it("hook: desmontar não deixa timer nem listener", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    const view = render(<Composer />);

    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem" }));
    resize(470);
    expect(vi.getTimerCount()).toBe(1);
    view.unmount();

    expect(vi.getTimerCount()).toBe(0);
    expect(vv?.removed).toContain("resize");
    resize(400);
    expect(vi.getTimerCount()).toBe(0);
    advance(KEYBOARD_SETTLE_MS * 2);
    expect(calls).toHaveLength(0);
  });

  it("perguntas: o campo 'Outro' focado vai à vista", () => {
    useFakeTimers();
    setMobile(true);
    const calls = spyScroll();
    render(
      <ChoicesForm
        questions={[{ id: "q1", text: "Banco?", options: ["Postgres"], multi: false }]}
        answered={null}
        onSubmit={() => undefined}
      />,
    );
    fireEvent.click(screen.getByLabelText("Outro"));
    const other = screen.getByRole("textbox", { name: "Outro: Banco?" });

    fireEvent.focusIn(other);
    resize(470);
    advance(KEYBOARD_RESIZE_DEBOUNCE_MS);

    expect(calls).toEqual([{ el: other, arg: { block: "nearest" } }]);
  });
});

describe("conversa no fim continua no fim quando a altura muda", () => {
  it("caixa encolhe (teclado) e a rolagem segue o fim", () => {
    const callbacks: (() => void)[] = [];
    class FakeResizeObserver {
      constructor(cb: () => void) {
        callbacks.push(cb);
      }
      observe() {}
      disconnect() {}
    }
    vi.stubGlobal("ResizeObserver", FakeResizeObserver);
    render(
      <AutoScrollBox items={[{ id: 1 }]} label="Conversa" className="nd-scroll">
        <p>oi</p>
      </AutoScrollBox>,
    );
    const box = screen.getByRole("region", { name: "Conversa" });
    let client = 600;
    let top = 0;
    Object.defineProperty(box, "scrollHeight", { configurable: true, get: () => 1000 });
    Object.defineProperty(box, "clientHeight", { configurable: true, get: () => client });
    Object.defineProperty(box, "scrollTop", { configurable: true, get: () => top, set: (v: number) => (top = Math.min(v, 1000 - client)) });
    box.scrollTop = 1000;
    fireEvent.scroll(box);
    expect(box.scrollTop).toBe(400);

    client = 300;
    act(() => callbacks.forEach((cb) => cb()));

    expect(box.scrollTop).toBe(700);
    vi.unstubAllGlobals();
  });
});
