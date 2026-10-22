/*
 * Modo celular (spec 093, T10; spec 083): com o teclado
 * aberto, a barra compacta e as abas se escondem (`.nd-keyboard-hide`) e o campo fica visível.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { KEYBOARD_AREA_ATTR, MOBILE_QUERY, installKeyboardMode, useKeyboardMode } from "./viewport";

class FakeVisualViewport extends EventTarget {
  height = 800;
  width = 390;
  scale = 1;
  listeners = 0;
  override addEventListener(type: string, listener: EventListenerOrEventListenerObject | null): void {
    this.listeners += 1;
    super.addEventListener(type, listener);
  }
  override removeEventListener(type: string, listener: EventListenerOrEventListenerObject | null): void {
    this.listeners -= 1;
    super.removeEventListener(type, listener);
  }
}

let vv: FakeVisualViewport;

function setMobile(mobile: boolean): void {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: query === MOBILE_QUERY ? mobile : false,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

const mark = (): string | undefined => document.documentElement.dataset.keyboard;

function resize(height: number, width = vv.width): void {
  act(() => {
    vv.height = height;
    vv.width = width;
    vv.dispatchEvent(new Event("resize"));
  });
}

function Screen() {
  useKeyboardMode();
  return (
    <div>
      <header className="nd-topbar nd-keyboard-hide">Projeto</header>
      <input aria-label="Busca" type="text" />
      <div {...{ [KEYBOARD_AREA_ATTR]: "" }} className="nd-composer-dock">
        <textarea aria-label="Mensagem para o Arquiteto" />
        <button type="submit">Enviar</button>
      </div>
    </div>
  );
}

beforeEach(() => {
  vv = new FakeVisualViewport();
  Object.defineProperty(window, "visualViewport", { configurable: true, writable: true, value: vv });
});

afterEach(() => {
  cleanup();
  delete document.documentElement.dataset.keyboard;
  Reflect.deleteProperty(window, "visualViewport");
});

describe("modo teclado aberto (spec 083)", () => {
  it("celular: foco na caixa + altura < 75% liga; altura > 90% desliga", () => {
    setMobile(true);
    render(<Screen />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });

    fireEvent.focusIn(field);
    expect(mark()).toBeUndefined();
    resize(700);
    expect(mark()).toBeUndefined();
    resize(470);
    expect(mark()).toBe("open");
    // Histerese: entre 75% e 90% mantém.
    resize(680);
    expect(mark()).toBe("open");
    resize(760);
    expect(mark()).toBeUndefined();
  });

  it("perder o foco desliga; foco que sai da área desliga", () => {
    setMobile(true);
    render(<Screen />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });
    const send = screen.getByRole("button", { name: "Enviar" });

    fireEvent.focusIn(field);
    resize(470);
    expect(mark()).toBe("open");
    fireEvent.focusOut(field, { relatedTarget: null });
    expect(mark()).toBeUndefined();

    fireEvent.focusIn(field);
    expect(mark()).toBe("open");
    fireEvent.focusOut(field, { relatedTarget: send });
    fireEvent.focusIn(send);
    expect(mark()).toBeUndefined();
  });

  it("sem foco, ou com foco fora da área de conversa, não liga", () => {
    setMobile(true);
    render(<Screen />);
    resize(470);
    expect(mark()).toBeUndefined();
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Busca" }));
    resize(460);
    expect(mark()).toBeUndefined();
  });

  it("desktop: nunca liga", () => {
    setMobile(false);
    render(<Screen />);
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" }));
    resize(300);
    expect(mark()).toBeUndefined();
  });

  it("rotação recomeça a referência de altura", () => {
    setMobile(true);
    render(<Screen />);
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" }));
    resize(390, 844);
    expect(mark()).toBeUndefined();
  });

  it("desmontar remove listeners e a marca", () => {
    setMobile(true);
    const view = render(<Screen />);
    const docRemove = vi.spyOn(document, "removeEventListener");
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" }));
    resize(470);
    expect(mark()).toBe("open");
    expect(vv.listeners).toBe(1);

    view.unmount();

    expect(mark()).toBeUndefined();
    expect(vv.listeners).toBe(0);
    expect(docRemove.mock.calls.map((c) => c[0])).toEqual(expect.arrayContaining(["focusin", "focusout"]));
  });

  it("sem visualViewport não lança e não marca", () => {
    Reflect.deleteProperty(window, "visualViewport");
    expect(() => installKeyboardMode()()).not.toThrow();
    expect(mark()).toBeUndefined();
  });
});

describe("CSS do modo teclado aberto", () => {
  const css = readFileSync(resolve(process.cwd(), "src/desktop/theme/mobile.css"), "utf8");

  it("esconde .nd-keyboard-hide só no modo celular e só com a marca", () => {
    const rule = /:root\[data-keyboard="open"\] \.nd-root\[data-mobile="true"\] \.nd-keyboard-hide \{\s*display: none !important;\s*\}/;
    expect(css).toMatch(rule);
  });

  it("no modo celular a altura acompanha a viewport visual (--app-height)", () => {
    expect(css).toMatch(/\.nd-root\[data-mobile="true"\] \{[^}]*height: var\(--app-height, 100%\);/);
  });
});
