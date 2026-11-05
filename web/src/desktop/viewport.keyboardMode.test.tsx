/*
 * Modo celular (spec 093, T10; spec 083; 141/T11): com o teclado aberto, a barra compacta, as abas
 * (`.nd-keyboard-hide`) e o cabeçalho da janela se escondem, e o campo fica visível. Num aparelho de
 * toque, o foco na caixa já liga o modo.
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

const TOUCH = "(hover: none) and (pointer: coarse)";

function setMobile(mobile: boolean, touch = false): void {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: query === MOBILE_QUERY ? mobile : query === TOUCH ? touch : false,
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

  it("aparelho de toque: o foco na caixa liga sem esperar a viewport encolher (Chrome do iPhone, 141/T11)", () => {
    setMobile(true, true);
    render(<Screen />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });

    fireEvent.focusIn(field);
    expect(mark()).toBe("open");
    // A largura muda junto com o teclado: a referência recomeça, e o modo continua ligado.
    resize(420, 389);
    expect(mark()).toBe("open");
    fireEvent.focusOut(field, { relatedTarget: null });
    expect(mark()).toBeUndefined();
  });

  it("aparelho de toque: o teclado fecha com o foco no campo e desliga; encolher ou focar de novo religa", () => {
    setMobile(true, true);
    render(<Screen />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });

    fireEvent.focusIn(field);
    resize(470);
    expect(mark()).toBe("open");
    resize(780);
    expect(mark()).toBeUndefined();
    // Entre as duas frações, continua desligado (histerese).
    resize(700);
    expect(mark()).toBeUndefined();
    resize(460);
    expect(mark()).toBe("open");
    resize(790);
    expect(mark()).toBeUndefined();
    fireEvent.focusIn(field);
    expect(mark()).toBe("open");
  });

  it("aparelho de toque: foco fora da área, ou no desktop, não liga", () => {
    setMobile(true, true);
    render(<Screen />);
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Busca" }));
    expect(mark()).toBeUndefined();
    cleanup();
    setMobile(false, true);
    render(<Screen />);
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" }));
    expect(mark()).toBeUndefined();
  });

  it("aparelho de toque sem visualViewport: o foco liga e a saída desliga", () => {
    Reflect.deleteProperty(window, "visualViewport");
    setMobile(true, true);
    render(<Screen />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });
    fireEvent.focusIn(field);
    expect(mark()).toBe("open");
    fireEvent.focusOut(field, { relatedTarget: null });
    expect(mark()).toBeUndefined();
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

  it("esconde o cabeçalho e as ferramentas da janela com o foco, só no celular e com a marca (141/T11)", () => {
    const rule = /:root\[data-keyboard="open"\] \.nd-root\[data-mobile="true"\] \.nd-window:focus-within > :where\(\.nd-titlebar, \.nd-window-tools\) \{\s*display: none !important;\s*\}/;
    expect(css).toMatch(rule);
  });

  it("no modo celular a altura acompanha a viewport visual (--app-height)", () => {
    expect(css).toMatch(/\.nd-root\[data-mobile="true"\] \{[^}]*height: var\(--app-height, 100%\);/);
  });
});
