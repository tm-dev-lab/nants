import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { useReducer, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ANIMATION_MS } from "./animation";
import { windowTitleBarId, windowTitleId } from "./focus";
import { findForbiddenGlyphs } from "./glyphs";
import { fakeResizeObservers, installPointerCapturePolyfill, manualScheduler, pointerEvent } from "./test-utils";
import { RESIZE_CORNER_PX, RESIZE_EDGE_PX, Window, type WindowProps } from "./Window";
import { DEFAULT_WINDOW_HOST, WindowHostContext, type WindowHost } from "./windowHost";
import { KEYBOARD_STEP } from "./windowMenu";
import { windowSubtitleId } from "./windowMeta";
import { createDesktopState, windowsReducer, type WindowAction, type WindowState } from "./windows";

afterEach(() => cleanup());

const WS = { w: 1200, h: 800 };
const BASE: WindowState = { x: 100, y: 80, w: 480, h: 360, z: 1, mode: "normal", open: true };

interface Harness {
  actions: WindowAction[];
  announcements: string[];
  host: WindowHost;
}

function makeHost(overrides: Partial<WindowHost> = {}): Harness {
  const actions: WindowAction[] = [];
  const announcements: string[] = [];
  const host: WindowHost = {
    ...DEFAULT_WINDOW_HOST,
    act: (a) => actions.push(a),
    announce: (m) => announcements.push(m),
    ...overrides,
  };
  return { actions, announcements, host };
}

function renderWindow(props: Partial<WindowProps> = {}, overrides: Partial<WindowHost> = {}) {
  const harness = makeHost(overrides);
  const view = render(
    <WindowHostContext.Provider value={harness.host}>
      <Window kind="plan" win={BASE} workspace={WS} active {...props} />
    </WindowHostContext.Provider>,
  );
  return { ...harness, ...view };
}

/** Janela ligada ao reducer de verdade (para ver o efeito das ações). */
function Live({ initial, host, children }: { initial: WindowState; host?: Partial<WindowHost>; children?: ReactNode }) {
  const [state, dispatch] = useReducer(windowsReducer, undefined, () =>
    createDesktopState({ windows: { plan: initial } }, WS),
  );
  const win = state.layout.windows.plan;
  if (win === undefined) return null;
  return (
    <WindowHostContext.Provider value={{ ...DEFAULT_WINDOW_HOST, act: dispatch, ...host }}>
      <Window kind="plan" win={win} workspace={WS} active>
        {children}
      </Window>
    </WindowHostContext.Provider>
  );
}

describe("Window: região, barra e botões", () => {
  it("é uma região rotulada pelo título e descrita pelo subtítulo vivo", () => {
    renderWindow({ subtitle: "001-orbita · aprovado" });
    const region = screen.getByRole("region", { name: "Plano" });
    expect(region.getAttribute("aria-labelledby")).toBe(windowTitleId("plan"));
    expect(region.getAttribute("aria-describedby")).toBe(windowSubtitleId("plan"));
    expect(document.getElementById(windowSubtitleId("plan"))?.textContent).toBe("001-orbita · aprovado");
    const bar = document.getElementById(windowTitleBarId("plan"));
    expect(bar?.hasAttribute("data-window-titlebar")).toBe(true);
    expect(bar?.tabIndex).toBe(0);
    expect(bar?.getAttribute("aria-keyshortcuts")).toBe("Shift+F10 ContextMenu");
    expect(region.querySelector("[data-window-content]")).not.toBeNull();
    expect(bar?.querySelector("[data-icon='plan']")).not.toBeNull();
  });

  it("menu, minimizar, maximizar e fechar: ícone SVG e aria-label, sem caracteres", () => {
    const { actions, container } = renderWindow();
    const region = screen.getByRole("region", { name: "Plano" });
    for (const [label, icon] of [
      ["Menu da janela", "more"],
      ["Minimizar", "minimize"],
      ["Maximizar", "maximize"],
      ["Fechar", "close"],
    ] as const) {
      const button = within(region).getByRole("button", { name: label });
      expect(button.querySelector(`svg[data-icon='${icon}']`)).not.toBeNull();
      expect(button.textContent).toBe("");
    }
    fireEvent.click(within(region).getByRole("button", { name: "Minimizar" }));
    fireEvent.click(within(region).getByRole("button", { name: "Maximizar" }));
    fireEvent.click(within(region).getByRole("button", { name: "Fechar" }));
    expect(actions).toEqual([
      { type: "minimize", kind: "plan" },
      { type: "toggleMaximize", kind: "plan" },
      { type: "close", kind: "plan" },
    ]);
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });

  it("maximizada: o botão vira Restaurar e as alças somem", () => {
    renderWindow({ win: { ...BASE, mode: "maximized" } });
    const region = screen.getByRole("region", { name: "Plano" });
    const restore = within(region).getByRole("button", { name: "Restaurar" });
    expect(restore.querySelector("svg[data-icon='restore']")).not.toBeNull();
    expect(region.querySelector("[data-handle]")).toBeNull();
    expect(region.style.width).toBe(`${WS.w}px`);
  });

  it("duplo clique na barra maximiza; nos botões, não", () => {
    const { actions } = renderWindow();
    const bar = document.getElementById(windowTitleBarId("plan"));
    if (bar === null) throw new Error("barra ausente");
    fireEvent.doubleClick(bar);
    fireEvent.doubleClick(within(bar).getByRole("button", { name: "Fechar" }));
    expect(actions).toEqual([{ type: "toggleMaximize", kind: "plan" }]);
  });

  it("alças de redimensionar com ≥ 10 px de área útil (borda direita, inferior e canto)", () => {
    renderWindow();
    const region = screen.getByRole("region", { name: "Plano" });
    const e = region.querySelector<HTMLElement>("[data-handle='e']");
    const s = region.querySelector<HTMLElement>("[data-handle='s']");
    const se = region.querySelector<HTMLElement>("[data-handle='se']");
    expect(RESIZE_EDGE_PX).toBeGreaterThanOrEqual(10);
    expect(RESIZE_CORNER_PX).toBeGreaterThanOrEqual(10);
    expect(e?.style.width).toBe(`${RESIZE_EDGE_PX}px`);
    expect(s?.style.height).toBe(`${RESIZE_EDGE_PX}px`);
    expect(se?.style.width).toBe(`${RESIZE_CORNER_PX}px`);
    expect(se?.style.height).toBe(`${RESIZE_CORNER_PX}px`);
    expect(se?.querySelector("svg[data-icon='resize']")).not.toBeNull();
  });
});

describe("Window: estado vazio e conteúdo", () => {
  it("sem conteúdo, mostra o estado vazio padrão do tipo", () => {
    renderWindow();
    expect(screen.getByText("O Arquiteto ainda não montou o plano. Descreva o que você quer na Conversa.")).toBeTruthy();
  });

  it("estado vazio com ação", () => {
    const onClick = vi.fn();
    renderWindow({ empty: { message: "Nada aqui.", action: { label: "Abrir a Conversa", onClick } } });
    fireEvent.click(screen.getByRole("button", { name: "Abrir a Conversa" }));
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("o conteúdo recebe o tamanho do contêiner (useContainerSize)", () => {
    const observers = fakeResizeObservers();
    renderWindow({ children: (size) => <p>tamanho {size}</p> }, { createObserver: observers.factory });
    expect(screen.getByText("tamanho wide")).toBeTruthy();
    act(() => observers.resize(400));
    expect(screen.getByText("tamanho narrow")).toBeTruthy();
    expect(document.querySelector("[data-window-content]")?.getAttribute("data-size")).toBe("narrow");
  });
});

describe("Window: foco e ocultação", () => {
  it("focusin numa janela que não está na frente a traz para a frente e é lembrado", () => {
    const remembered: HTMLElement[] = [];
    const { actions } = renderWindow(
      { active: false, children: <input aria-label="campo" /> },
      { rememberFocus: (_k, el) => remembered.push(el) },
    );
    const input = screen.getByRole("textbox", { name: "campo" });
    act(() => input.focus());
    expect(actions).toContainEqual({ type: "focus", kind: "plan" });
    expect(remembered).toContain(input);
  });

  it("minimizada fica hidden + inert, sem aria-hidden", () => {
    renderWindow({ win: { ...BASE, mode: "minimized", restoreMode: "normal" } });
    const region = document.querySelector<HTMLElement>("[data-window='plan']");
    expect(region?.hidden).toBe(true);
    expect(region?.hasAttribute("inert")).toBe(true);
    expect(region?.hasAttribute("aria-hidden")).toBe(false);
  });
});

describe("Window: menu da janela (alternativa de ponteiro único)", () => {
  it("abre pelo botão, navega por setas, escolhe e fecha", () => {
    const { actions } = renderWindow();
    const button = screen.getByRole("button", { name: "Menu da janela" });
    fireEvent.click(button);
    const menu = screen.getByRole("menu", { name: "Menu da janela Plano" });
    expect(button.getAttribute("aria-expanded")).toBe("true");
    const items = within(menu).getAllByRole("menuitem");
    expect(items.map((i) => i.textContent)).toEqual([
      "Encaixar à esquerda",
      "Encaixar à direita",
      "Quarto superior esquerdo",
      "Quarto superior direito",
      "Quarto inferior esquerdo",
      "Quarto inferior direito",
      "Centralizar",
      "Compacta",
      "Média",
      "Ampla",
      "Mover pelo teclado",
      "Redimensionar pelo teclado",
      "Maximizar",
      "Minimizar",
      "Fechar",
    ]);
    expect(document.activeElement).toBe(items[0]);
    fireEvent.keyDown(menu, { key: "ArrowDown" });
    expect(document.activeElement).toBe(items[1]);
    fireEvent.keyDown(menu, { key: "End" });
    expect(document.activeElement).toBe(items[items.length - 1]);
    fireEvent.click(within(menu).getByRole("menuitem", { name: "Encaixar à direita" }));
    expect(actions).toEqual([{ type: "snap", kind: "plan", target: "right" }]);
    expect(screen.queryByRole("menu")).toBeNull();
    expect(findForbiddenGlyphs(document.body)).toEqual([]);
  });

  it("Shift+F10 e a tecla Menu na barra abrem o menu; Esc fecha e devolve o foco à barra", async () => {
    renderWindow();
    const bar = document.getElementById(windowTitleBarId("plan"));
    if (bar === null) throw new Error("barra ausente");
    act(() => bar.focus());
    fireEvent.keyDown(bar, { key: "F10", shiftKey: true });
    const menu = screen.getByRole("menu");
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    await act(async () => {
      await Promise.resolve();
    });
    expect(document.activeElement).toBe(bar);
    fireEvent.keyDown(bar, { key: "ContextMenu" });
    expect(screen.getByRole("menu")).toBeTruthy();
  });

  it("maximizada, o item vira Restaurar", () => {
    renderWindow({ win: { ...BASE, mode: "maximized" } });
    fireEvent.click(screen.getByRole("button", { name: "Menu da janela" }));
    expect(screen.getByRole("menuitem", { name: "Restaurar" })).toBeTruthy();
  });

  it("clique fora fecha o menu", () => {
    renderWindow();
    fireEvent.click(screen.getByRole("button", { name: "Menu da janela" }));
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("mover pelo teclado: setas em passos de 16 px, Enter confirma", () => {
    render(<Live initial={BASE} />);
    fireEvent.click(screen.getByRole("button", { name: "Menu da janela" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Mover pelo teclado" }));
    const bar = document.getElementById(windowTitleBarId("plan"));
    const region = document.querySelector<HTMLElement>("[data-window='plan']");
    if (bar === null || region === null) throw new Error("janela ausente");
    expect(document.activeElement).toBe(bar);
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    fireEvent.keyDown(bar, { key: "ArrowDown" });
    expect(region.style.transform).toBe(`translate(${KEYBOARD_STEP}px, ${KEYBOARD_STEP}px)`);
    expect(region.style.left).toBe(`${BASE.x}px`);
    fireEvent.keyDown(bar, { key: "Enter" });
    expect(region.style.transform).toBe("");
    expect(region.style.left).toBe(`${BASE.x + KEYBOARD_STEP}px`);
    expect(region.style.top).toBe(`${BASE.y + KEYBOARD_STEP}px`);
  });

  it("redimensionar pelo teclado: Esc cancela e restaura", () => {
    render(<Live initial={BASE} />);
    fireEvent.click(screen.getByRole("button", { name: "Menu da janela" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Redimensionar pelo teclado" }));
    const bar = document.getElementById(windowTitleBarId("plan"));
    const region = document.querySelector<HTMLElement>("[data-window='plan']");
    if (bar === null || region === null) throw new Error("janela ausente");
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    expect(region.style.width).toBe(`${BASE.w + KEYBOARD_STEP}px`);
    fireEvent.keyDown(bar, { key: "Escape" });
    expect(region.style.width).toBe(`${BASE.w}px`);
  });
});

describe("Window: animações FLIP", () => {
  let animate: ReturnType<typeof vi.fn>;
  let restorePrototype: () => void;

  beforeEach(() => {
    animate = vi.fn();
    const proto = HTMLElement.prototype as HTMLElement & { animate?: unknown };
    const original = Object.getOwnPropertyDescriptor(proto, "animate");
    Object.defineProperty(proto, "animate", { configurable: true, writable: true, value: animate });
    restorePrototype = () => {
      if (original === undefined) delete (proto as { animate?: unknown }).animate;
      else Object.defineProperty(proto, "animate", original);
    };
  });
  afterEach(() => restorePrototype());

  it("maximizar e restaurar interpolam com transform (animationFor)", () => {
    render(<Live initial={BASE} />);
    fireEvent.click(screen.getByRole("button", { name: "Maximizar" }));
    expect(animate).toHaveBeenCalledTimes(1);
    const [keyframes, options] = animate.mock.calls[0] as [Keyframe[], KeyframeAnimationOptions];
    expect(String(keyframes[0]?.transform)).toMatch(/^translate\(100px, 80px\) scale\(/);
    expect(keyframes[1]?.transform).toBe("translate(0px, 0px) scale(1, 1)");
    expect(options.duration).toBe(ANIMATION_MS.geometry);
    fireEvent.click(screen.getByRole("button", { name: "Restaurar" }));
    expect(animate).toHaveBeenCalledTimes(2);
  });

  it("mudar a área de trabalho (resize do navegador) não anima a geometria", () => {
    function Resizable() {
      const [state, dispatch] = useReducer(windowsReducer, undefined, () =>
        createDesktopState({ windows: { plan: { ...BASE, mode: "maximized" } } }, WS),
      );
      const current = state.layout.windows.plan;
      if (current === undefined) return null;
      return (
        <WindowHostContext.Provider value={{ ...DEFAULT_WINDOW_HOST, act: dispatch }}>
          <button type="button" onClick={() => dispatch({ type: "setWorkspace", workspace: { w: 900, h: 600 } })}>
            encolher
          </button>
          <Window kind="plan" win={current} workspace={state.workspace} active />
        </WindowHostContext.Provider>
      );
    }
    render(<Resizable />);
    fireEvent.click(screen.getByRole("button", { name: "encolher" }));
    expect(document.querySelector<HTMLElement>("[data-window='plan']")?.style.width).toBe("900px");
    expect(animate).not.toHaveBeenCalled();
    // Uma mudança de geometria depois disso volta a animar.
    fireEvent.click(screen.getByRole("button", { name: "Restaurar" }));
    expect(animate).toHaveBeenCalledTimes(1);
  });

  it("prefers-reduced-motion desliga as animações", () => {
    render(<Live initial={BASE} host={{ reducedMotion: () => true }} />);
    fireEvent.click(screen.getByRole("button", { name: "Maximizar" }));
    expect(animate).not.toHaveBeenCalled();
  });

  it("minimizar anima até o ícone do dock, e a janela só some depois (nunca presa)", async () => {
    vi.useFakeTimers();
    try {
      const dock = document.createElement("button");
      dock.id = "dock-plan";
      document.body.append(dock);
      render(<Live initial={BASE} />);
      fireEvent.click(screen.getByRole("button", { name: "Minimizar" }));
      const region = document.querySelector<HTMLElement>("[data-window='plan']");
      expect(animate).toHaveBeenCalledTimes(1);
      const [keyframes] = animate.mock.calls[0] as [Keyframe[]];
      expect(keyframes[1]?.opacity).toBe(0);
      // Estado já minimizado: inerte na hora; visível só durante a animação.
      expect(region?.hasAttribute("inert")).toBe(true);
      expect(region?.hidden).toBe(false);
      await act(async () => {
        vi.advanceTimersByTime(ANIMATION_MS.minimize + 200);
      });
      expect(region?.hidden).toBe(true);
      dock.remove();
    } finally {
      vi.useRealTimers();
    }
  });

  it("fim de um gesto de arrastar não anima (a janela já está no lugar)", () => {
    const restoreCapture = installPointerCapturePolyfill();
    const scheduler = manualScheduler();
    try {
      render(<Live initial={BASE} host={{ scheduler }} />);
      const bar = document.getElementById(windowTitleBarId("plan"));
      const region = document.querySelector<HTMLElement>("[data-window='plan']");
      if (bar === null || region === null) throw new Error("janela ausente");
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 300, clientY: 300 }));
        bar.dispatchEvent(pointerEvent("pointermove", { clientX: 340, clientY: 320 }));
        scheduler.runFrames();
      });
      expect(region.style.transform).toBe("translate(40px, 20px)");
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerup", { clientX: 340, clientY: 320 }));
      });
      expect(region.style.left).toBe(`${BASE.x + 40}px`);
      expect(region.style.transform).toBe("");
      // A geometria inline continua inteira depois da prévia (largura e altura não somem).
      expect(region.style.width).toBe(`${BASE.w}px`);
      expect(region.style.height).toBe(`${BASE.h}px`);
      expect(animate).not.toHaveBeenCalled();
    } finally {
      restoreCapture();
    }
  });

  it("redimensionar pelo canto e cancelar devolve a largura e a altura do estado", () => {
    const restoreCapture = installPointerCapturePolyfill();
    const scheduler = manualScheduler();
    try {
      render(<Live initial={BASE} host={{ scheduler }} />);
      const region = document.querySelector<HTMLElement>("[data-window='plan']");
      const corner = region?.querySelector<HTMLElement>("[data-handle='se']");
      if (region === null || corner === null || corner === undefined) throw new Error("janela ausente");
      act(() => {
        corner.dispatchEvent(pointerEvent("pointerdown", { clientX: 580, clientY: 440 }));
        corner.dispatchEvent(pointerEvent("pointermove", { clientX: 640, clientY: 480 }));
        scheduler.runFrames();
      });
      expect(region.style.width).toBe(`${BASE.w + 60}px`);
      act(() => {
        corner.dispatchEvent(pointerEvent("pointercancel", { clientX: 640, clientY: 480 }));
      });
      expect(region.style.width).toBe(`${BASE.w}px`);
      expect(region.style.height).toBe(`${BASE.h}px`);
    } finally {
      restoreCapture();
    }
  });
});
