/*
 * Barra de título de 44 px e slot de ferramentas (095/T02, C-06 e C-07):
 * - `TITLEBAR_HEIGHT` (menu da janela) e o `--titlebar-h` do CSS vêm da mesma fonte; a alça leste
 *   começa abaixo da barra;
 * - o slot `[data-window-tools]` fica entre o subtítulo e os controles; o arrasto não começa nele,
 *   os atalhos da barra (Shift+F10) não valem lá dentro e o Tab entra nas ferramentas;
 * - `<WindowTools>` põe as ferramentas de um app no slot da janela que o contém;
 * - no celular o slot é uma linha logo abaixo da barra (fora dela), rolável na horizontal.
 */
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { useReducer, type ReactNode } from "react";
import { afterEach, describe, expect, it } from "vitest";
import { windowTitleBarId } from "./focus";
import { TITLEBAR_HEIGHT } from "./metrics";
import { shortcutContext } from "./shortcuts";
import { cssRuleBody, cssRules, installPointerCapturePolyfill, manualScheduler, pointerEvent } from "./test-utils";
import { Window, WindowTools } from "./Window";
import { DEFAULT_WINDOW_HOST, WindowHostContext, type WindowHost } from "./windowHost";
import { createDesktopState, windowsReducer, type WindowAction, type WindowState } from "./windows";

afterEach(() => cleanup());

const WS = { w: 1200, h: 800 };
const BASE: WindowState = { x: 100, y: 80, w: 480, h: 360, z: 1, mode: "normal", open: true };
const css = (name: string): string => readFileSync(resolve(process.cwd(), `src/desktop/theme/${name}`), "utf8");

function Tools() {
  return (
    <>
      <button type="button">Conexões</button>
      <input aria-label="Endereço" defaultValue="192.168.0.68:4173" />
    </>
  );
}

/** Janela ligada ao reducer de verdade (o arrasto muda a posição do estado). */
function Live({ host, tools, children }: { host?: Partial<WindowHost>; tools?: ReactNode; children?: ReactNode }) {
  const [state, dispatch] = useReducer(windowsReducer, undefined, () => createDesktopState({ windows: { settings: BASE } }, WS));
  const win = state.layout.windows.settings;
  if (win === undefined) return null;
  return (
    <WindowHostContext.Provider value={{ ...DEFAULT_WINDOW_HOST, act: dispatch, ...host }}>
      <Window kind="settings" win={win} workspace={WS} active tools={tools}>
        {children}
      </Window>
    </WindowHostContext.Provider>
  );
}

const bar = (): HTMLElement => {
  const el = document.getElementById(windowTitleBarId("settings"));
  if (el === null) throw new Error("barra ausente");
  return el;
};

describe("barra de título de 44 px (C-06)", () => {
  it("TITLEBAR_HEIGHT é o --titlebar-h da raiz; a barra e a alça leste usam a variável", () => {
    expect(TITLEBAR_HEIGHT).toBe(44);
    const desktop = css("desktop.css");
    expect(cssRuleBody(desktop, ".nd-root")).toContain(`--titlebar-h: ${TITLEBAR_HEIGHT}px;`);
    expect(cssRuleBody(desktop, ".nd-titlebar")).toContain("height: var(--titlebar-h);");
    expect(cssRuleBody(desktop, '.nd-handle[data-handle="e"]')).toContain("top: var(--titlebar-h);");
    // Nenhuma altura de barra de título solta no desktop (o celular tem a própria, de toque).
    const loose = cssRules(desktop).filter((r) => r.selectors.includes(".nd-titlebar") && /height:\s*\d/.test(r.body));
    expect(loose).toEqual([]);
  });

  it("o menu da janela abre logo abaixo da barra (TITLEBAR_HEIGHT)", () => {
    const layer = document.createElement("div");
    document.body.append(layer);
    try {
      render(<Live host={{ menuLayer: layer }} />);
      fireEvent.click(screen.getByRole("button", { name: "Menu da janela" }));
      const menu = layer.querySelector<HTMLElement>("[role='menu']");
      expect(menu?.style.top).toBe(`${BASE.y + TITLEBAR_HEIGHT}px`);
    } finally {
      layer.remove();
    }
  });

  it("título 13/600 (--fs-control), subtítulo --muted e controles só com ícone", () => {
    const desktop = css("desktop.css");
    expect(cssRuleBody(desktop, ".nd-title")).toContain("font-size: var(--fs-control);");
    expect(cssRuleBody(desktop, ".nd-title")).toContain("font-weight: var(--fw-semibold);");
    expect(cssRuleBody(desktop, ".nd-subtitle")).toContain("color: var(--muted);");
    const wbtn = cssRuleBody(desktop, ".nd-wbtn");
    expect(wbtn).toContain("background: transparent;");
    expect(wbtn).toContain("border: 0;");
  });
});

describe("controles sempre à direita (137/T04)", () => {
  const controls = (): string[] =>
    [...bar().querySelectorAll(":scope > button")].map((b) => `${b.getAttribute("aria-label") ?? ""}${b.hasAttribute("data-window-controls-start") ? " [start]" : ""}`);

  it("o primeiro controle leva a margem automática, com ou sem ferramentas no slot", () => {
    expect(cssRuleBody(css("desktop.css"), ".nd-titlebar > [data-window-controls-start]")).toContain("margin-left: auto;");
    // Sem ferramentas (o App em modo Console, Rodando, Terminou): o slot some e nada mais empurra.
    render(<Live />);
    expect(bar().querySelector("[data-window-tools]")?.childElementCount).toBe(0);
    expect(controls()).toEqual(["Menu da janela [start]", "Minimizar", "Maximizar", "Fechar"]);
    cleanup();
    render(<Live tools={<Tools />} />);
    expect(controls()).toEqual(["Menu da janela [start]", "Minimizar", "Maximizar", "Fechar"]);
    expect(bar().querySelector("[data-window-tools]")?.nextElementSibling?.hasAttribute("data-window-controls-start")).toBe(true);
  });

  it("no celular, o Fechar (único controle) também fica na ponta", () => {
    render(<Live host={{ mobile: true }} />);
    expect(controls()).toEqual(["Fechar [start]"]);
  });

  it("nenhuma regra de janela tira a margem dos controles", () => {
    for (const file of ["desktop.css", "mobile.css"]) {
      const loose = cssRules(css(file)).filter((r) => r.selectors.some((sel) => /\.nd-wbtn|data-window-controls-start/.test(sel)) && /margin(-left)?:/.test(r.body));
      expect(loose.map((r) => r.selectors.join(", "))).toEqual(file === "desktop.css" ? [".nd-titlebar > [data-window-controls-start]"] : []);
    }
  });
});

describe("slot de ferramentas [data-window-tools] (C-07)", () => {
  it("fica entre o subtítulo e os controles; vazio, some", () => {
    render(<Live tools={<Tools />} />);
    const slot = bar().querySelector<HTMLElement>("[data-window-tools]");
    if (slot === null) throw new Error("slot ausente");
    const subtitle = bar().querySelector(".nd-subtitle");
    expect(slot.previousElementSibling).toBe(subtitle);
    expect(slot.nextElementSibling?.getAttribute("aria-label")).toBe("Menu da janela");
    expect(within(slot).getByRole("button", { name: "Conexões" })).toBeTruthy();
    expect(cssRuleBody(css("desktop.css"), ".nd-window-tools:empty")).toContain("display: none;");
  });

  it("arrastar a partir do input do slot não move a janela; da barra, move", () => {
    const restore = installPointerCapturePolyfill();
    const scheduler = manualScheduler();
    try {
      render(<Live host={{ scheduler }} tools={<Tools />} />);
      const region = document.querySelector<HTMLElement>("[data-window='settings']");
      const input = screen.getByRole("textbox", { name: "Endereço" });
      if (region === null) throw new Error("janela ausente");
      act(() => {
        input.dispatchEvent(pointerEvent("pointerdown", { clientX: 300, clientY: 100 }));
        input.dispatchEvent(pointerEvent("pointermove", { clientX: 360, clientY: 140 }));
        scheduler.runFrames();
        input.dispatchEvent(pointerEvent("pointerup", { clientX: 360, clientY: 140 }));
      });
      expect(region.style.transform).toBe("");
      expect(region.style.left).toBe(`${BASE.x}px`);
      // O espaço vazio do slot também não arrasta.
      const slot = bar().querySelector<HTMLElement>("[data-window-tools]");
      act(() => {
        slot?.dispatchEvent(pointerEvent("pointerdown", { clientX: 300, clientY: 100 }));
        slot?.dispatchEvent(pointerEvent("pointermove", { clientX: 360, clientY: 140 }));
        scheduler.runFrames();
        slot?.dispatchEvent(pointerEvent("pointerup", { clientX: 360, clientY: 140 }));
      });
      expect(region.style.left).toBe(`${BASE.x}px`);
      // A barra continua arrastando.
      act(() => {
        bar().dispatchEvent(pointerEvent("pointerdown", { clientX: 300, clientY: 100 }));
        bar().dispatchEvent(pointerEvent("pointermove", { clientX: 340, clientY: 120 }));
        scheduler.runFrames();
        bar().dispatchEvent(pointerEvent("pointerup", { clientX: 340, clientY: 120 }));
      });
      expect(region.style.left).toBe(`${BASE.x + 40}px`);
    } finally {
      restore();
    }
  });

  it("duplo clique no slot não maximiza", () => {
    const actions: WindowAction[] = [];
    render(
      <WindowHostContext.Provider value={{ ...DEFAULT_WINDOW_HOST, act: (a) => actions.push(a) }}>
        <Window kind="settings" win={BASE} workspace={WS} active tools={<Tools />} />
      </WindowHostContext.Provider>,
    );
    fireEvent.doubleClick(screen.getByRole("textbox", { name: "Endereço" }));
    expect(actions).toEqual([]);
    fireEvent.doubleClick(bar());
    expect(actions).toEqual([{ type: "toggleMaximize", kind: "settings" }]);
  });

  it("Tab entra nas ferramentas, depois da barra e antes dos controles da janela", async () => {
    const user = userEvent.setup();
    render(<Live tools={<Tools />} />);
    bar().focus();
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Conexões" }));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Endereço" }));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Menu da janela" }));
  });

  it("os atalhos da barra (Shift+F10) não valem dentro do slot: shortcutContext.onTitleBar exclui o slot", () => {
    render(<Live tools={<Tools />} />);
    expect(shortcutContext(bar()).onTitleBar).toBe(true);
    expect(shortcutContext(screen.getByRole("button", { name: "Menu da janela" })).onTitleBar).toBe(true);
    const tool = screen.getByRole("button", { name: "Conexões" });
    expect(shortcutContext(tool).onTitleBar).toBe(false);
    tool.focus();
    fireEvent.keyDown(tool, { key: "F10", shiftKey: true });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("<WindowTools> leva as ferramentas do app para o slot da janela", () => {
    render(
      <Live>
        <p>conteúdo</p>
        <WindowTools>
          <button type="button">Voltar</button>
        </WindowTools>
      </Live>,
    );
    const slot = bar().querySelector<HTMLElement>("[data-window-tools]");
    expect(slot?.contains(screen.getByRole("button", { name: "Voltar" }))).toBe(true);
    expect(document.querySelector("[data-window-content]")?.contains(screen.getByRole("button", { name: "Voltar" }))).toBe(false);
  });

  it("<WindowTools> fora de uma janela não desenha nada", () => {
    render(
      <WindowTools>
        <button type="button">Solto</button>
      </WindowTools>,
    );
    expect(screen.queryByRole("button", { name: "Solto" })).toBeNull();
  });

  it("celular: o slot é uma linha de titânio abaixo da barra (fora dela), rolável na horizontal", () => {
    render(<Live host={{ mobile: true }} tools={<Tools />} />);
    expect(bar().querySelector("[data-window-tools]")).toBeNull();
    const row = document.querySelector<HTMLElement>("[data-window-tools]");
    expect(row?.getAttribute("data-row")).toBe("true");
    expect(row?.previousElementSibling).toBe(bar());
    expect(row?.nextElementSibling?.hasAttribute("data-window-content")).toBe(true);
    expect(row?.classList.contains("nd-chrome")).toBe(true);
    const mobile = cssRuleBody(css("mobile.css"), '.nd-root[data-mobile="true"] .nd-window-tools');
    expect(mobile).toContain("overflow-x: auto;");
    expect(mobile).toContain("flex-shrink: 0;");
  });
});
