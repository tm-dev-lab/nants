/*
 * Menu do N (spec 137, T01; `reference/menu-n.dc.html`): o botão N na barra de cima e o menu que
 * ele abre. Abrir e fechar (clique, Alt+0, Esc, véu, clique fora, escolha), foco e Tab preso,
 * busca com setas e Enter, grade com os `kinds` do Desktop e os estados dos apps, selos e o ponto
 * do N, janelas abertas, Alt+1…9 com o menu aberto, altura limitada e movimento reduzido. O jsdom
 * não aplica CSS: as medidas e a animação são lidas no CSS-fonte.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { BRAND_NAME, BRAND_SLOGAN } from "./brand";
import { Desktop, useDesktopReducer, type DesktopProps } from "./Desktop";
import { windowTitleBarId } from "./focus";
import { MIN_DESKTOP_HEIGHT, TOPBAR_HEIGHT } from "./metrics";
import { NMENU_ID, pendingCount } from "./NMenu";
import { activateApp, renderShell, settle } from "./shell-test-utils";
import { makeProject, makeProjectState } from "../test-utils";
import { shortcutById, SHORTCUTS } from "./shortcuts";
import { cssRuleBody, cssRules, fakeUiStateServer } from "./test-utils";
import { SPACE, THEME_NAMES, themeVariables } from "./theme/tokens";
import { useUiState } from "./useUiState";
import type { Layout, WindowState } from "./windows";

const desktopCss = readFileSync(resolve(process.cwd(), "src/desktop/theme/desktop.css"), "utf8");

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const WS = { w: 1200, h: 800 };
const win = (patch: Partial<WindowState> = {}): WindowState => ({ x: 40, y: 40, w: 480, h: 360, z: 1, mode: "normal", open: true, ...patch });

function setup(options: { layout?: Layout; props?: Partial<DesktopProps> } = {}) {
  const server = fakeUiStateServer({ rev: 1, data: {} });
  const deps = {
    fetchImpl: server.fetchImpl,
    storage: null,
    createChannel: null,
    setTimer: (fn: () => void) => setTimeout(fn, 0),
    clearTimer: (handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };
  function Harness(props: Partial<DesktopProps>) {
    const globalState = useUiState({ kind: "global" }, deps);
    const [state, dispatch] = useDesktopReducer(options.layout ?? { windows: {} }, WS);
    return <Desktop state={state} dispatch={dispatch} globalState={globalState} {...options.props} {...props} />;
  }
  const view = render(<Harness />);
  return { rerender: (props: Partial<DesktopProps>) => view.rerender(<Harness {...props} />) };
}

const nButton = (): HTMLButtonElement => {
  const el = document.querySelector<HTMLButtonElement>("[data-nbutton]");
  if (el === null) throw new Error("botão N ausente");
  return el;
};
const menu = (): HTMLElement | null => document.querySelector<HTMLElement>("[data-nmenu]");
const openMenu = (): HTMLElement => {
  fireEvent.click(nButton());
  const el = menu();
  if (el === null) throw new Error("menu ausente");
  return el;
};
const search = (): HTMLInputElement => document.querySelector<HTMLInputElement>("[data-nmenu-search]") as HTMLInputElement;
const tile = (kind: string): HTMLElement => document.querySelector<HTMLElement>(`.nd-nmenu-tile[data-kind="${kind}"]`) as HTMLElement;
const tileKinds = (): (string | null)[] => [...document.querySelectorAll(".nd-nmenu-tile")].map((el) => el.getAttribute("data-kind"));
const altZero = (target: Element = document.body): void => {
  fireEvent.keyDown(target, { key: "0", code: "Digit0", altKey: true });
};

describe("botão N", () => {
  it("começa fechado; botão real com nome, aria-expanded, aria-haspopup e Alt+0", () => {
    setup();
    expect(menu()).toBeNull();
    const button = nButton();
    expect(button.tagName).toBe("BUTTON");
    expect(button.getAttribute("aria-label")).toBe("Menu do Nants");
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(button.getAttribute("aria-haspopup")).toBe("dialog");
    expect(button.getAttribute("aria-keyshortcuts")).toBe("Alt+0");
    expect(button.hasAttribute("aria-controls")).toBe(false);
    // Fica no lugar da marca: o primeiro item da barra de cima, antes do conteúdo do shell.
    expect(document.querySelector("header.nd-topbar")?.firstElementChild).toBe(button);
  });

  it("o símbolo é o NantsSymbol oficial (as três peças do asset da marca), sem mudança", () => {
    setup();
    const symbol = nButton().querySelector("svg[data-brand-symbol]");
    const asset = readFileSync(resolve(process.cwd(), "public/brand/nants-symbol-color-dark.svg"), "utf8");
    const assetPaths = [...asset.matchAll(/<path fill="([^"]+)" d="([^"]+)"/g)].map((m) => [m[1], m[2]]);
    const drawn = [...(symbol?.querySelectorAll("path") ?? [])].map((p) => [p.getAttribute("fill"), p.getAttribute("d")]);
    expect(assetPaths).toHaveLength(3);
    expect(drawn).toEqual(assetPaths);
  });

  it("ponto rosa: soma só os selos dos apps, com o texto acessível; some com o menu aberto", () => {
    const windows = {
      plan: { badge: { count: 1, label: "plano pronto" } },
      conversation: { badge: { count: 2, label: "2 novas" } },
      costs: { badge: { count: 0, label: "nada" } },
    };
    setup({ props: { windows } });
    expect(nButton().querySelector("[data-nbutton-dot]")).not.toBeNull();
    expect(nButton().getAttribute("aria-label")).toBe("Menu do Nants, 3 novidades nos aplicativos");
    openMenu();
    expect(nButton().querySelector("[data-nbutton-dot]")).toBeNull();
    expect(pendingCount(["plan"], { plan: { count: 1, label: "x" } })).toBe(1);
    expect(pendingCount(["costs"], { plan: { count: 4, label: "x" } })).toBe(0);
  });

  it("sem selo, sem ponto (e o nome no singular quando há um)", () => {
    const view = setup();
    expect(nButton().querySelector("[data-nbutton-dot]")).toBeNull();
    view.rerender({ windows: { plan: { badge: { count: 1, label: "plano pronto" } } } });
    expect(nButton().getAttribute("aria-label")).toBe("Menu do Nants, 1 novidade nos aplicativos");
  });

  it("CSS: até 36 px de altura dentro da barra de 44; relevo; afunda aberto, sem contorno colorido; foco visível", () => {
    const body = cssRuleBody(desktopCss, ".nd-nbtn");
    const height = Number(/(?:^|;)\s*height:\s*(\d+)px/.exec(body)?.[1]);
    expect(height).toBeLessThanOrEqual(36);
    expect(height).toBeLessThan(TOPBAR_HEIGHT);
    expect(body).toContain("background: var(--nbtn-bg);");
    expect(body).toContain("box-shadow: var(--elev-nbtn);");
    const open = cssRuleBody(desktopCss, '.nd-nbtn[aria-expanded="true"]');
    expect(open).toContain("background: var(--nbtn-bg-open);");
    expect(open).toContain("box-shadow: var(--elev-nbtn-open);");
    expect(open).not.toMatch(/outline|border|accent|violet/);
    for (const theme of THEME_NAMES) {
      const vars = themeVariables(theme);
      // Afundado: só sombra interna (mais a borda de luz embaixo), nenhuma cor de destaque.
      expect(vars["--elev-nbtn-open"]).toMatch(/^inset /);
      expect(vars["--elev-nbtn"]).toContain("inset 0 1px 0");
      for (const name of ["--nbtn-bg", "--nbtn-bg-open", "--ndot", "--ring-ndot", "--elev-nmenu", "--nmenu-scrim"] as const) {
        expect(vars[name]).toBeTruthy();
      }
    }
    const focus = cssRules(desktopCss).find((r) => r.selectors.includes(".nd-root .nd-nbtn:focus-visible"));
    expect(focus?.body).toContain("outline: 2px solid var(--focus-outer)");
  });
});

describe("abrir e fechar", () => {
  it("clique abre: diálogo modal com aria-controls; foco na busca; clique no N fecha e o foco fica nele", () => {
    setup();
    const panel = openMenu();
    expect(panel.getAttribute("role")).toBe("dialog");
    expect(panel.getAttribute("aria-modal")).toBe("true");
    expect(panel.getAttribute("aria-label")).toBe("Menu do Nants");
    expect(nButton().getAttribute("aria-expanded")).toBe("true");
    expect(nButton().getAttribute("aria-controls")).toBe(NMENU_ID);
    expect(panel.id).toBe(NMENU_ID);
    expect(document.activeElement).toBe(search());
    fireEvent.click(nButton());
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(nButton());
  });

  it("topo: símbolo, Nants, o slogan logo depois e a versão em mono", () => {
    setup({ props: { version: "0.1.0-rc.21" } });
    const panel = openMenu();
    const head = panel.querySelector<HTMLElement>("[data-nmenu-head]");
    expect(head?.querySelector("svg[data-brand-symbol]")).not.toBeNull();
    const name = within(head as HTMLElement).getByText(BRAND_NAME);
    const slogan = within(head as HTMLElement).getByText(BRAND_SLOGAN);
    expect(name.nextElementSibling).toBe(slogan);
    expect(slogan.nextElementSibling).toBeNull();
    const version = head?.querySelector("[data-nmenu-version]");
    expect(version?.textContent).toBe("0.1.0-rc.21");
    expect(version?.getAttribute("aria-label")).toBe("Versão 0.1.0-rc.21");
    expect(cssRuleBody(desktopCss, ".nd-nmenu-version")).toContain("font-family: var(--font-mono);");
  });

  it("Alt+0 abre e fecha (o menu trata o atalho com o modal aberto); o foco volta ao N", () => {
    setup();
    altZero();
    expect(menu()).not.toBeNull();
    expect(document.activeElement).toBe(search());
    altZero(search());
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(nButton());
    // Alt+0 pelo `code` físico: um layout que troca o `key` também abre.
    fireEvent.keyDown(document.body, { key: "º", code: "Digit0", altKey: true });
    expect(menu()).not.toBeNull();
  });

  it("Esc fecha e devolve o foco ao N", () => {
    setup();
    openMenu();
    fireEvent.keyDown(search(), { key: "Escape" });
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(nButton());
  });

  it("clique no véu fecha (foco no N); clique fora, na barra, também fecha", () => {
    setup();
    openMenu();
    const scrim = document.querySelector<HTMLElement>("[data-nmenu-scrim]");
    expect(scrim?.getAttribute("aria-hidden")).toBe("true");
    fireEvent.mouseDown(scrim as HTMLElement);
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(nButton());
    openMenu();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Personalizar" }));
    expect(menu()).toBeNull();
    // Dentro do menu não fecha.
    openMenu();
    fireEvent.pointerDown(search());
    expect(menu()).not.toBeNull();
  });

  it("o Tab fica preso no menu", () => {
    setup({ layout: { windows: { conversation: win() } } });
    const panel = openMenu();
    const controls = [...panel.querySelectorAll<HTMLElement>("button, input")];
    const last = controls[controls.length - 1] as HTMLElement;
    last.focus();
    fireEvent.keyDown(last, { key: "Tab" });
    expect(document.activeElement).toBe(search());
    fireEvent.keyDown(search(), { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("o lançador pede o menu (sinal); o celular não tem N nem menu", () => {
    const view = setup();
    view.rerender({ nMenuSignal: 1 });
    expect(menu()).not.toBeNull();
    cleanup();
    setup({ props: { mobile: { view: "switcher", tabs: ["switcher"], closeView: "switcher", onView: () => undefined } } });
    expect(document.querySelector("[data-nbutton]")).toBeNull();
    altZero();
    expect(menu()).toBeNull();
  });

  it("abrir o menu fecha a folha de atalhos e o Personalizar", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Personalizar" }));
    expect(screen.getByRole("button", { name: "Personalizar" }).getAttribute("aria-expanded")).toBe("true");
    openMenu();
    expect(screen.getByRole("button", { name: "Personalizar" }).getAttribute("aria-expanded")).toBe("false");
  });
});

describe("busca e grade", () => {
  it("a grade usa os `kinds` do Desktop (sem projeto: Configurações e Primeiros passos)", () => {
    setup({ props: { appKinds: ["settings", "setup"] } });
    openMenu();
    expect(tileKinds()).toEqual(["settings", "setup"]);
    expect(screen.getByRole("list", { name: "Aplicativos" })).toBeTruthy();
  });

  it("filtra enquanto se digita (sem acento nem caixa); setas movem o destaque; Enter abre e fecha o menu", () => {
    setup();
    openMenu();
    fireEvent.change(search(), { target: { value: "CONFIG" } });
    expect(tileKinds()).toEqual(["settings"]);
    expect(search().getAttribute("aria-activedescendant")).toBe("nmenu-settings");
    fireEvent.change(search(), { target: { value: "execucao" } });
    expect(tileKinds()).toEqual(["execution"]);
    fireEvent.change(search(), { target: { value: "" } });
    expect(search().hasAttribute("aria-activedescendant")).toBe(false);
    fireEvent.keyDown(search(), { key: "ArrowRight" });
    expect(tile("conversation").getAttribute("data-highlight")).toBe("true");
    fireEvent.keyDown(search(), { key: "ArrowRight" });
    expect(search().getAttribute("aria-activedescendant")).toBe("nmenu-plan");
    fireEvent.keyDown(search(), { key: "ArrowDown" });
    // Quatro colunas: uma linha abaixo do Plano é o App.
    expect(search().getAttribute("aria-activedescendant")).toBe("nmenu-app");
    fireEvent.keyDown(search(), { key: "ArrowUp" });
    fireEvent.keyDown(search(), { key: "ArrowLeft" });
    expect(search().getAttribute("aria-activedescendant")).toBe("nmenu-conversation");
    fireEvent.keyDown(search(), { key: "Enter" });
    expect(menu()).toBeNull();
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("sem resultado, a frase do dicionário", () => {
    setup();
    openMenu();
    fireEvent.change(search(), { target: { value: "zzz" } });
    expect(tileKinds()).toEqual([]);
    expect(menu()?.textContent).toContain("Nenhum aplicativo com esse nome");
  });

  it("setas e Home/End com o foco na grade", () => {
    setup();
    openMenu();
    tile("conversation").focus();
    fireEvent.keyDown(tile("conversation"), { key: "ArrowRight" });
    expect(document.activeElement).toBe(tile("plan"));
    fireEvent.keyDown(tile("plan"), { key: "ArrowDown" });
    expect(document.activeElement).toBe(tile("app"));
    fireEvent.keyDown(tile("app"), { key: "End" });
    expect(document.activeElement).toBe(tile("settings"));
    fireEvent.keyDown(tile("settings"), { key: "Home" });
    expect(document.activeElement).toBe(tile("conversation"));
  });

  it("os estados dos apps: fechado, aberto com o ponto, minimizado e em foco; dica e estado no aria-describedby", () => {
    setup({
      layout: { windows: { conversation: win({ z: 1 }), board: win({ z: 3 }), plan: win({ z: 2, mode: "minimized" }) } },
      props: { windows: { board: { subtitle: "4 tarefas" }, plan: { badge: { count: 2, label: "2 pendências" } } } },
    });
    openMenu();
    expect(tile("activity").getAttribute("data-state")).toBe("closed");
    expect(tile("activity").querySelector(".nd-nmenu-indicator")).toBeNull();
    expect(tile("conversation").getAttribute("data-state")).toBe("open");
    expect(tile("conversation").querySelector('[data-indicator="open"]')).not.toBeNull();
    expect(tile("plan").getAttribute("data-state")).toBe("minimized");
    expect(tile("board").getAttribute("data-state")).toBe("focused");
    const tip = (kind: string): string => document.getElementById(tile(kind).getAttribute("aria-describedby") ?? "")?.textContent ?? "";
    expect(tip("board")).toBe("em foco, 4 tarefas");
    expect(tip("plan")).toBe("minimizada, 2 pendências");
    expect(tip("activity")).toBe("fechada");
    // Selo: o mesmo das abas do celular, só ele com cor.
    expect(tile("plan").querySelector("[data-badge]")?.textContent).toBe("2");
    expect(tile("plan").getAttribute("aria-keyshortcuts")).toBe("Alt+2");
    expect(tile("plan").getAttribute("aria-label")).toBe("Plano");
    // CSS: fechado em --muted, aberto em --ink, em foco com o pressionado.
    expect(cssRuleBody(desktopCss, ".nd-nmenu-icon")).toContain("color: var(--muted);");
    const rules = cssRules(desktopCss);
    expect(rules.find((r) => r.selectors.includes('.nd-nmenu-tile[data-state="open"] .nd-nmenu-icon'))?.body).toContain("color: var(--ink);");
    const focused = rules.filter((r) => r.selectors.includes('.nd-nmenu-tile[data-state="focused"] .nd-nmenu-icon')).map((r) => r.body).join(";");
    expect(focused).toContain("background-color: var(--state-press);");
  });
});

describe("ativar", () => {
  it("item fechado abre a janela e foca a barra de título; o menu fecha", () => {
    setup({ layout: { windows: { conversation: win() } } });
    activateApp("Plano");
    expect(menu()).toBeNull();
    expect(document.activeElement?.id).toBe(windowTitleBarId("plan"));
  });

  it("a janela em foco é minimizada pela grade; de novo, restaurada", () => {
    setup({ layout: { windows: { conversation: win() } } });
    activateApp("Conversa");
    expect((document.querySelector('[data-window="conversation"]') as HTMLElement).hidden).toBe(true);
    activateApp("Conversa");
    expect((document.querySelector('[data-window="conversation"]') as HTMLElement).hidden).toBe(false);
  });

  it("janelas abertas: a linha das abertas e minimizadas; um clique restaura (sem minimizar a em foco)", () => {
    setup({ layout: { windows: { conversation: win({ z: 2 }), plan: win({ z: 1, mode: "minimized" }) } } });
    const panel = openMenu();
    const chips = panel.querySelector<HTMLElement>("[data-nmenu-open]");
    expect([...(chips?.querySelectorAll("button") ?? [])].map((b) => b.getAttribute("aria-label"))).toEqual([
      "Conversa, em foco",
      "Plano, minimizada",
    ]);
    expect(panel.textContent).toContain("Janelas abertas · 2");
    fireEvent.click(within(chips as HTMLElement).getByRole("button", { name: "Plano, minimizada" }));
    expect(menu()).toBeNull();
    expect((document.querySelector('[data-window="plan"]') as HTMLElement).hidden).toBe(false);
    openMenu();
    fireEvent.click(screen.getByRole("button", { name: "Plano, em foco" }));
    expect((document.querySelector('[data-window="plan"]') as HTMLElement).hidden).toBe(false);
  });

  it("Alt+1…9 com o menu aberto abre direto e fecha o menu", () => {
    setup();
    openMenu();
    fireEvent.keyDown(search(), { key: "2", code: "Digit2", altKey: true });
    expect(menu()).toBeNull();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
  });

  it("rodapé com os atalhos", () => {
    setup();
    const foot = openMenu().querySelector("[data-nmenu-foot]");
    expect(foot?.textContent).toContain("Alt+1 a Alt+9 abrem direto");
    expect(foot?.textContent).toContain("Esc fecha");
  });
});

describe("altura limitada à área de trabalho", () => {
  /** Partes fixas do menu (px), lidas do CSS: o resto da altura é da grade, que rola. */
  const px = (selector: string, prop: string): number => Number(new RegExp(`(?:^|;)\\s*${prop}:\\s*(\\d+)px`).exec(cssRuleBody(desktopCss, selector))?.[1]);

  it("o menu não passa da área de trabalho e só a grade rola", () => {
    const panel = cssRuleBody(desktopCss, ".nd-nmenu");
    expect(panel).toContain("max-height: calc(100% - var(--sp-1) - var(--sp-2));");
    expect(panel).toContain("display: flex;");
    expect(panel).toContain("flex-direction: column;");
    const grid = cssRuleBody(desktopCss, ".nd-nmenu > .nd-nmenu-grid");
    expect(grid).toContain("overflow-y: auto;");
    expect(grid).toContain("min-height: 0;");
    expect(cssRuleBody(desktopCss, ".nd-nmenu > *")).toContain("flex: 0 0 auto;");
    // A camada começa abaixo da barra (o véu não a cobre) e vai até a borda de baixo.
    const layer = cssRuleBody(desktopCss, ".nd-nmenu-layer");
    expect(layer).toContain("top: var(--topbar-h);");
    expect(layer).toContain("bottom: 0;");
  });

  it.each([
    [1366, 640],
    [1024, MIN_DESKTOP_HEIGHT],
  ])("%ix%i: as partes fixas cabem e sobra ao menos uma linha da grade", (_w, h) => {
    const available = h - TOPBAR_HEIGHT - SPACE[1] - SPACE[2];
    const gap = SPACE[3];
    const padding = 2 * SPACE[3];
    const head = 28;
    const searchBox = px(".nd-nmenu-search", "height");
    const openWindows = 16 + SPACE[2] + px(".nd-nmenu-chip", "height");
    const foot = SPACE[2] + 16;
    const fixed = padding + 4 * gap + head + searchBox + openWindows + foot;
    const row = px(".nd-nmenu-grid", "grid-auto-rows");
    expect(available - fixed).toBeGreaterThanOrEqual(row);
    // Na menor altura do desktop, com todos os apps (quatro linhas), a grade passa e rola.
    if (h === MIN_DESKTOP_HEIGHT) expect(fixed + 4 * row + 3 * SPACE[2]).toBeGreaterThan(available);
  });
});

describe("animação, véu e movimento reduzido", () => {
  it("desce ao abrir (0,26 s, revelando de cima para baixo, conteúdo em cascata) e sobe ao fechar", () => {
    expect(cssRuleBody(desktopCss, ".nd-nmenu")).toMatch(/animation: nd-nmenu-drop 0\.26s/);
    expect(desktopCss).toMatch(/@keyframes nd-nmenu-drop \{\s*from \{[^}]*clip-path: inset\(0 0 100% 0/);
    expect(cssRuleBody(desktopCss, '.nd-nmenu-layer[data-state="closing"] .nd-nmenu')).toContain("animation: nd-nmenu-lift");
    expect(cssRuleBody(desktopCss, ".nd-nmenu > :nth-child(5)")).toContain("animation-delay");
    const scrim = cssRuleBody(desktopCss, ".nd-nmenu-scrim");
    expect(scrim).toContain("background: var(--nmenu-scrim);");
    expect(scrim).toMatch(/backdrop-filter: blur\(\d+px\)/);
  });

  it("com movimento reduzido: sem animação nem desfoque", () => {
    const reduced = cssRules(desktopCss).filter((r) => r.at !== null && /prefers-reduced-motion: reduce/.test(r.at));
    const none = reduced.find((r) => r.selectors.includes(".nd-nmenu") && r.selectors.includes(".nd-nmenu-scrim"));
    expect(none?.body).toContain("animation: none");
    expect(none?.selectors).toEqual(expect.arrayContaining([".nd-nmenu > *", '.nd-nmenu-layer[data-state="closing"] .nd-nmenu']));
    expect(reduced.find((r) => r.selectors.includes(".nd-nmenu-scrim") && r.body.includes("backdrop-filter: none"))).toBeDefined();
  });

  it("fechando com animação: inerte e fora da árvore acessível até as animações terminarem; com movimento reduzido, sai na hora", async () => {
    let finish: () => void = () => undefined;
    const finished = new Promise<void>((resolve) => {
      finish = resolve;
    });
    // jsdom não tem `getAnimations`: uma animação falsa, terminada quando o teste quiser.
    Object.defineProperty(HTMLElement.prototype, "getAnimations", { configurable: true, value: () => [{ finished }] });
    try {
      setup();
      openMenu();
      fireEvent.keyDown(search(), { key: "Escape" });
      const closing = menu();
      expect(closing).not.toBeNull();
      expect(closing?.getAttribute("aria-hidden")).toBe("true");
      expect(closing?.hasAttribute("inert")).toBe(true);
      expect(closing?.hasAttribute("aria-modal")).toBe(false);
      expect(document.querySelector("[data-nmenu-layer]")?.getAttribute("data-state")).toBe("closing");
      expect(nButton().getAttribute("aria-expanded")).toBe("false");
      await act(async () => {
        finish();
        await finished;
      });
      expect(menu()).toBeNull();

      // Movimento reduzido: sem fase de saída.
      vi.stubGlobal("matchMedia", (query: string) => ({ matches: query.includes("reduce"), media: query, addEventListener: () => undefined, removeEventListener: () => undefined }));
      openMenu();
      fireEvent.keyDown(search(), { key: "Escape" });
      expect(menu()).toBeNull();
    } finally {
      Reflect.deleteProperty(HTMLElement.prototype, "getAnimations");
    }
  });
});

describe("tabela de atalhos", () => {
  it("o menu do N está na tabela única, como Alt+0, e aparece na folha de atalhos", () => {
    expect(shortcutById("nMenu")).toMatchObject({ aria: "Alt+0", keys: "Alt+0" });
    expect(SHORTCUTS.map((s) => s.id)).toContain("nMenu");
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Atalhos do teclado" }));
    const sheet = document.querySelector<HTMLElement>("[data-shortcut-sheet]");
    expect(sheet?.textContent).toContain("Alt+0");
    expect(sheet?.textContent).toContain("Abrir e fechar o menu do Nants");
  });

  it("no lançador: 'Abrir o menu do Nants' com Alt+0 abre o menu", async () => {
    await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const launcher = screen.getByRole("dialog", { name: "Lançador" });
    fireEvent.change(within(launcher).getByRole("combobox"), { target: { value: "menu do nants" } });
    const item = launcher.querySelector<HTMLElement>('[data-launcher-item="open-nmenu"]');
    expect(item?.textContent).toContain("Alt+0");
    fireEvent.keyDown(within(launcher).getByRole("combobox"), { key: "Enter" });
    await settle();
    expect(screen.queryByRole("dialog", { name: "Lançador" })).toBeNull();
    expect(menu()).not.toBeNull();
    expect(document.activeElement).toBe(search());
  });
});
