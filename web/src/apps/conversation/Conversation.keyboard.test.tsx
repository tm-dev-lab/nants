/* Janela Conversa (spec 093, T06). Spec 082/T06: ao focar a caixa do Arquiteto no celular, campo, anexar e enviar ficam à vista. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { useAppStore } from "../../store";
import type { ChatMessage } from "../../store";
import { ATTACH_LABEL } from "./ChatAttachments";
import { KEYBOARD_RESIZE_DEBOUNCE_MS, MOBILE_QUERY } from "../../desktop/viewport";

vi.mock("../../api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api/client")>()),
  postChat: vi.fn(),
}));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

const NONE: ChatMessage[] = [];

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

let calls: { el: Element; arg: unknown }[] = [];
const vv = new EventTarget();
/** Quadros pendentes (rAF contável): a revelação roda depois do quadro do `--app-height`. */
let frames: FrameRequestCallback[] = [];

/** Teclado subindo em degraus a cada 40 ms; depois espera o debounce e roda os quadros. */
function keyboardOpens(): void {
  for (let step = 0; step < 5; step += 1) {
    act(() => {
      vv.dispatchEvent(new Event("resize"));
      vi.advanceTimersByTime(40);
    });
  }
  act(() => {
    vi.advanceTimersByTime(KEYBOARD_RESIZE_DEBOUNCE_MS);
    const pending = frames;
    frames = [];
    pending.forEach((cb) => cb(0));
  });
}

beforeEach(() => {
  calls = [];
  frames = [];
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval"] });
  vi.spyOn(window, "requestAnimationFrame").mockImplementation((cb) => {
    frames.push(cb);
    return frames.length;
  });
  vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => undefined);
  Object.defineProperty(window, "visualViewport", { configurable: true, writable: true, value: vv });
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    configurable: true,
    writable: true,
    value: function (this: Element, arg: unknown) {
      calls.push({ el: this, arg });
    },
  });
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  Reflect.deleteProperty(window, "visualViewport");
  Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
});

function setMobile(mobile: boolean): void {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: query === MOBILE_QUERY ? mobile : false,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

describe("caixa do Arquiteto e teclado virtual (spec 082/T06)", () => {
  it("celular: foco + resize do teclado revela o contêiner com campo, anexar e enviar", () => {
    setMobile(true);
    render(<Harness id="kb-mobile" />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });

    fireEvent.focusIn(field);
    keyboardOpens();

    const revealed = calls.filter((c) => c.el.contains(field));
    expect(revealed).toHaveLength(1);
    const dock = revealed[0]?.el;
    expect(revealed[0]?.arg).toEqual({ block: "nearest" });
    expect(dock?.classList.contains("nd-composer-dock")).toBe(true);
    expect(dock?.contains(screen.getByRole("button", { name: ATTACH_LABEL }))).toBe(true);
    expect(dock?.contains(screen.getByRole("button", { name: "Enviar" }))).toBe(true);
  });

  it("placeholder curto que cabe no celular; nome acessível completo", () => {
    setMobile(true);
    render(<Harness id="kb-placeholder" />);
    const field = screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" });
    expect(field.getAttribute("placeholder")).toBe("Mensagem ao Arquiteto");
    expect(field.getAttribute("aria-label")).toBe("Mensagem para o Arquiteto");
  });

  it("desktop: foco não rola nada", () => {
    setMobile(false);
    render(<Harness id="kb-desktop" />);
    fireEvent.focusIn(screen.getByRole("textbox", { name: "Mensagem para o Arquiteto" }));
    keyboardOpens();
    expect(calls.filter((c) => c.el.classList.contains("nd-composer-dock"))).toHaveLength(0);
  });
});
