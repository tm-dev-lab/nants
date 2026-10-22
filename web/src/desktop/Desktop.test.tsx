import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Desktop, useDesktopReducer, type DesktopProps } from "./Desktop";
import { dockButtonId, windowTitleBarId } from "./focus";
import { gestureSignal } from "./gestures";
import { findForbiddenGlyphs } from "./glyphs";
import { HINT_IDS, HINTS, MAX_HINTS, markHintSeen, parseHints, shouldShowHint } from "./hints";
import { SHORTCUTS } from "./shortcuts";
import { fakeUiStateServer, installPointerCapturePolyfill, manualScheduler, pointerEvent } from "./test-utils";
import { THEMES } from "./theme/tokens";
import { useUiState } from "./useUiState";
import type { Layout, WindowState } from "./windows";

afterEach(() => {
  cleanup();
  gestureSignal.reset();
});

const WS = { w: 1200, h: 800 };
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

async function settle(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 20; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

function setup(options: { layout?: Layout; data?: Record<string, unknown>; props?: Partial<DesktopProps> } = {}) {
  const server = fakeUiStateServer({ rev: 1, data: options.data ?? {} });
  const deps = {
    fetchImpl: server.fetchImpl,
    storage: null,
    createChannel: null,
    setTimer: (fn: () => void) => setTimeout(fn, 0),
    clearTimer: (handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>),
  };
  function Harness() {
    const globalState = useUiState({ kind: "global" }, deps);
    const [state, dispatch] = useDesktopReducer(options.layout ?? { windows: {} }, WS);
    return <Desktop state={state} dispatch={dispatch} globalState={globalState} {...options.props} />;
  }
  const view = render(<Harness />);
  const status = (): string => document.querySelector("[data-desktop-status]")?.textContent?.trim() ?? "";
  const root = (): HTMLElement => {
    const el = view.container.querySelector<HTMLElement>(".nd-root");
    if (el === null) throw new Error("desktop ausente");
    return el;
  };
  return { server, status, root };
}

describe("Desktop: área de trabalho e área vazia", () => {
  it("sem janelas, a área de trabalho mostra só o papel de parede; o aviso existe só para leitor de tela", () => {
    setup();
    const work = screen.getByRole("main", { name: "Área de trabalho" });
    expect(work.querySelector("section, h2, button, kbd")).toBeNull();
    expect(document.querySelector("[data-desktop-empty]")).toBeNull();
    expect(screen.queryByRole("heading", { name: "Nenhuma janela aberta" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Restaurar o layout da fase" })).toBeNull();
    expect(findForbiddenGlyphs(work)).toEqual([]);
    const live = document.querySelector("[data-desktop-status]");
    expect(live?.getAttribute("aria-live")).toBe("polite");
    expect(live?.classList.contains("nd-sr-only")).toBe(true);
    expect(live?.textContent?.trim()).toBe("Nenhuma janela aberta. Use o dock ou Ctrl+K.");
    expect(document.querySelectorAll("[role='status']")).toHaveLength(1);
  });

  it("fechar a última janela leva o foco ao botão dela no dock e anuncia a área vazia", () => {
    const { status } = setup({ layout: { windows: { conversation: win() } } });
    expect(status()).toBe("");
    fireEvent.click(within(screen.getByRole("region", { name: "Conversa" })).getByRole("button", { name: "Fechar" }));
    expect(screen.queryByRole("region", { name: "Conversa" })).toBeNull();
    expect(document.activeElement?.id).toBe(dockButtonId("conversation"));
    expect(status()).toBe("Conversa fechada. Nenhuma janela aberta. Use o dock ou Ctrl+K.");
    expect(screen.getByRole("main", { name: "Área de trabalho" }).querySelector("section")).toBeNull();
  });

  it("fechar a última janela sem botão no dock leva o foco ao primeiro botão do dock", () => {
    setup({ layout: { windows: { conversation: win() } }, props: { dockKinds: ["plan", "board"] } });
    fireEvent.click(within(screen.getByRole("region", { name: "Conversa" })).getByRole("button", { name: "Fechar" }));
    expect(document.activeElement?.id).toBe(dockButtonId("plan"));
  });

  it("abrir uma janela pelo dock tira o aviso; minimizar todas deixa só o papel de parede", () => {
    const { status } = setup();
    fireEvent.click(screen.getByRole("button", { name: "Conversa" }));
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
    expect(status()).toBe("Conversa aberta");
    fireEvent.click(within(screen.getByRole("region", { name: "Conversa" })).getByRole("button", { name: "Minimizar" }));
    expect(status()).toBe("Conversa minimizada. Nenhuma janela aberta. Use o dock ou Ctrl+K.");
    expect(screen.getByRole("main", { name: "Área de trabalho" }).querySelector("section:not([data-window])")).toBeNull();
  });

  it("com conteúdo do shell (boas-vindas), o conteúdo aparece e não há aviso de área vazia", () => {
    const { status } = setup({ props: { emptyContent: <section data-welcome="">Boas-vindas</section> } });
    expect(document.querySelector("[data-welcome]")).toBeTruthy();
    expect(status()).toBe("");
  });

  it("só as janelas abertas do espaço são montadas, e o DOM nunca é reordenado ao focar", () => {
    setup({ layout: { windows: { conversation: win({ z: 1 }), plan: win({ x: 300, z: 2 }), board: win({ open: false }) } } });
    const order = (): string[] => Array.from(document.querySelectorAll("[data-window]")).map((el) => el.getAttribute("data-window") ?? "");
    expect(order()).toEqual(["conversation", "plan"]);
    const conversation = screen.getByRole("region", { name: "Conversa" });
    fireEvent.pointerDown(conversation);
    expect(order()).toEqual(["conversation", "plan"]);
    expect(Number(conversation.style.zIndex)).toBeGreaterThan(Number(screen.getByRole("region", { name: "Plano" }).style.zIndex));
  });

  it("estado vazio padrão da janela, com a ação que abre outra janela", () => {
    setup({ layout: { windows: { plan: win() } } });
    fireEvent.click(screen.getByRole("button", { name: "Abrir a Conversa" }));
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("o titânio está na barra superior, nas barras de título e no dock; os menus são sólidos", () => {
    setup({ layout: { windows: { plan: win() } } });
    expect(document.querySelector("header")?.classList.contains("nd-chrome")).toBe(true);
    expect(screen.getByRole("navigation", { name: "Dock" }).classList.contains("nd-chrome")).toBe(true);
    // Os botões do dock ficam sobre o titânio do dock (sem material próprio).
    expect(document.getElementById(dockButtonId("plan"))?.classList.contains("nd-chrome")).toBe(false);
    expect(document.getElementById(windowTitleBarId("plan"))?.classList.contains("nd-chrome")).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Menu da janela" }));
    const menu = screen.getByRole("menu");
    // O menu é sobreposição sólida, sem titânio (decisão do Arquiteto na revisão 1 da T01).
    expect(menu.classList.contains("nd-overlay")).toBe(true);
    expect(menu.classList.contains("nd-chrome")).toBe(false);
    // O menu é desenhado na camada de menus (fora da janela, que corta o que transborda).
    expect(menu.parentElement?.hasAttribute("data-menu-layer")).toBe(true);
    // A janela em si é sólida (sem titânio).
    expect(screen.getByRole("region", { name: "Plano" }).classList.contains("nd-chrome")).toBe(false);
  });
});

describe("Desktop: anúncios, foco e atalhos", () => {
  it("role=status fixo anuncia; minimizar leva o foco ao ícone do dock e restaurar volta ao último elemento", () => {
    const { status } = setup({
      layout: { windows: { conversation: win() } },
      props: { windows: { conversation: { content: <input aria-label="Mensagem" /> } } },
    });
    expect(document.querySelectorAll("[role='status']")).toHaveLength(1);
    const input = screen.getByRole("textbox", { name: "Mensagem" });
    act(() => input.focus());
    fireEvent.click(within(screen.getByRole("region", { name: "Conversa" })).getByRole("button", { name: "Minimizar" }));
    expect(status()).toBe("Conversa minimizada. Nenhuma janela aberta. Use o dock ou Ctrl+K.");
    expect(document.activeElement?.id).toBe(dockButtonId("conversation"));
    fireEvent.click(screen.getByRole("button", { name: "Conversa" }));
    expect(status()).toBe("Conversa restaurada");
    expect(document.activeElement).toBe(input);
  });

  it("fechar anuncia e leva o foco ao dock; maximizar anuncia", () => {
    const { status } = setup({ layout: { windows: { plan: win() } } });
    const region = screen.getByRole("region", { name: "Plano" });
    fireEvent.click(within(region).getByRole("button", { name: "Maximizar" }));
    expect(status()).toBe("Plano maximizada");
    fireEvent.click(within(region).getByRole("button", { name: "Fechar" }));
    expect(status()).toBe("Plano fechada. Nenhuma janela aberta. Use o dock ou Ctrl+K.");
    expect(document.activeElement?.id).toBe(dockButtonId("plan"));
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
  });

  it("Ctrl+Z com foco no desktop desfaz a última mudança de layout", () => {
    const { status } = setup({ layout: { windows: { plan: win() } } });
    fireEvent.click(screen.getByRole("button", { name: "Maximizar" }));
    expect(screen.getByRole("region", { name: "Plano" }).getAttribute("data-mode")).toBe("maximized");
    fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
    expect(screen.getByRole("region", { name: "Plano" }).getAttribute("data-mode")).toBe("normal");
    expect(status()).toBe("Última mudança de layout desfeita");
  });

  it("F6/Shift+F6 percorrem as janelas visíveis; Alt+N abre a janela da posição do dock", () => {
    setup({ layout: { windows: { conversation: win({ z: 1 }), board: win({ z: 2 }) } } });
    fireEvent.keyDown(document.body, { key: "F6" });
    // Da janela em foco (Quadro), a próxima na ordem do dock é a Conversa (com volta).
    expect(document.activeElement?.id).toBe(windowTitleBarId("conversation"));
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "F6" });
    expect(document.activeElement?.id).toBe(windowTitleBarId("board"));
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "F6", shiftKey: true });
    expect(document.activeElement?.id).toBe(windowTitleBarId("conversation"));
    // Alt+2 = Plano (segunda posição do dock).
    fireEvent.keyDown(document.body, { key: "2", code: "Digit2", altKey: true });
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    expect(document.activeElement?.id).toBe(windowTitleBarId("plan"));
  });

  it("095/T07: abrir pelo dock foca a barra SEM anel (foco programático); F6 e teclas mostram o anel neutro", () => {
    setup({ layout: { windows: { conversation: win({ z: 1 }) } } });
    fireEvent.click(document.getElementById(dockButtonId("plan")) as HTMLElement);
    const plan = document.getElementById(windowTitleBarId("plan"));
    expect(document.activeElement).toBe(plan);
    expect(plan?.getAttribute("data-quiet-focus")).toBe("true");
    // F6 é navegação real por teclado: a próxima barra recebe o foco com anel.
    fireEvent.keyDown(plan ?? document.body, { key: "F6" });
    const conversation = document.getElementById(windowTitleBarId("conversation"));
    expect(document.activeElement).toBe(conversation);
    expect(conversation?.hasAttribute("data-quiet-focus")).toBe(false);
    expect(plan?.hasAttribute("data-quiet-focus")).toBe(false);
  });

  it("Esc dentro da janela devolve o foco à barra de título e nunca fecha a janela", () => {
    setup({
      layout: { windows: { conversation: win() } },
      props: { windows: { conversation: { content: <button type="button">Ação</button> } } },
    });
    const button = screen.getByRole("button", { name: "Ação" });
    act(() => button.focus());
    fireEvent.keyDown(button, { key: "Escape" });
    expect(document.activeElement?.id).toBe(windowTitleBarId("conversation"));
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("Ctrl+K chama o lançador, até dentro de um campo", () => {
    const onOpenLauncher = vi.fn();
    setup({
      layout: { windows: { conversation: win() } },
      props: { onOpenLauncher, windows: { conversation: { content: <input aria-label="Mensagem" /> } } },
    });
    const input = screen.getByRole("textbox", { name: "Mensagem" });
    act(() => input.focus());
    fireEvent.keyDown(input, { key: "k", ctrlKey: true });
    expect(onOpenLauncher).toHaveBeenCalledTimes(1);
    // Outros atalhos não disparam dentro de campo.
    fireEvent.keyDown(input, { key: "?", shiftKey: true });
    expect(screen.queryByRole("dialog", { name: "Atalhos do teclado" })).toBeNull();
  });
});

describe("Desktop: folha de atalhos", () => {
  it("Shift+/ abre a folha gerada da tabela única; Esc fecha e devolve o foco", () => {
    setup({ layout: { windows: { plan: win() } } });
    const bar = document.getElementById(windowTitleBarId("plan"));
    if (bar === null) throw new Error("barra ausente");
    act(() => bar.focus());
    fireEvent.keyDown(bar, { key: "?", shiftKey: true, code: "Slash" });
    const sheet = screen.getByRole("dialog", { name: "Atalhos do teclado" });
    const keys = Array.from(sheet.querySelectorAll("kbd")).map((k) => k.textContent);
    expect(keys).toEqual(SHORTCUTS.map((s) => s.keys));
    for (const shortcut of SHORTCUTS) expect(within(sheet).getByText(shortcut.description)).toBeTruthy();
    expect(findForbiddenGlyphs(sheet)).toEqual([]);
    fireEvent.keyDown(sheet, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Atalhos do teclado" })).toBeNull();
    expect(document.activeElement).toBe(bar);
  });

  it("o botão de atalhos da barra também abre a folha", () => {
    setup();
    const button = screen.getByRole("button", { name: "Atalhos do teclado" });
    expect(button.getAttribute("aria-keyshortcuts")).toBe("Shift+/");
    fireEvent.click(button);
    expect(screen.getByRole("dialog", { name: "Atalhos do teclado" })).toBeTruthy();
  });
});

const metaContent = (name: string): string | null =>
  document.head.querySelector<HTMLMetaElement>(`meta[name="${name}"]`)?.content ?? null;

describe("Desktop: Personalizar (appearance)", () => {
  it("tema e papel de parede mudam na hora e são salvos na seção appearance", async () => {
    const { server, root } = setup();
    await settle();
    expect(root().getAttribute("data-theme")).toBe("dark");
    expect(root().style.getPropertyValue("--bg")).toBe(THEMES.dark.bg);
    expect(metaContent("theme-color")).toBe(THEMES.dark.bg);
    expect(metaContent("color-scheme")).toBe("dark");
    fireEvent.click(screen.getByRole("button", { name: "Personalizar" }));
    const dialog = screen.getByRole("dialog", { name: "Personalizar" });
    expect(within(dialog).getAllByRole("button", { pressed: true }).map((b) => b.textContent)).toEqual(["Escuro", "Pontos"]);
    fireEvent.click(within(dialog).getByRole("button", { name: "Claro" }));
    expect(root().getAttribute("data-theme")).toBe("light");
    expect(root().style.getPropertyValue("--bg")).toBe(THEMES.light.bg);
    // A barra do navegador no celular e os controles nativos acompanham o tema efetivo.
    expect(metaContent("theme-color")).toBe(THEMES.light.bg);
    expect(metaContent("color-scheme")).toBe("light");
    fireEvent.click(within(dialog).getByRole("button", { name: "Aurora" }));
    expect(root().getAttribute("data-wallpaper")).toBe("aurora");
    await settle();
    const puts = server.puts().map((c) => c.body);
    expect(puts[puts.length - 1]).toMatchObject({ section: "appearance", value: { theme: "light", wallpaper: "aurora" } });
    expect(findForbiddenGlyphs(dialog)).toEqual([]);
    fireEvent.keyDown(dialog, { key: "Escape" });
    expect(screen.queryByRole("dialog", { name: "Personalizar" })).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Personalizar" }));
  });

  it("lê a aparência salva; Automático segue o sistema", async () => {
    const original = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    })) as unknown as typeof window.matchMedia;
    try {
      const { root } = setup({ data: { appearance: { theme: "auto", wallpaper: "nebula" } } });
      await settle();
      expect(root().getAttribute("data-theme-preference")).toBe("auto");
      expect(root().getAttribute("data-theme")).toBe("light");
      expect(root().getAttribute("data-wallpaper")).toBe("nebula");
    } finally {
      window.matchMedia = original;
    }
  });
});

describe("Desktop: dicas contextuais (hints)", () => {
  it("no máximo duas dicas, cada uma lembrada em hints", () => {
    expect(HINT_IDS).toHaveLength(MAX_HINTS);
    expect(MAX_HINTS).toBeLessThanOrEqual(2);
    let state = parseHints(undefined);
    expect(shouldShowHint(state, "dock")).toBe(true);
    state = markHintSeen(state, "dock");
    expect(shouldShowHint(state, "dock")).toBe(false);
    expect(parseHints({ seen: ["dock", "x", 3] })).toEqual({ seen: ["dock"] });
  });

  it("primeira vez que minimiza: 'Suas janelas ficam no dock', salva em hints e não volta", async () => {
    const { server } = setup({ layout: { windows: { plan: win(), conversation: win({ z: 2 }) } } });
    await settle();
    fireEvent.click(within(screen.getByRole("region", { name: "Plano" })).getByRole("button", { name: "Minimizar" }));
    expect(screen.getByRole("note").textContent).toContain(HINTS.dock);
    await settle();
    expect(server.puts().map((c) => c.body)).toContainEqual({ base_rev: 1, section: "hints", value: { seen: ["dock"] } });
    fireEvent.click(screen.getByRole("button", { name: "Dispensar dica" }));
    expect(screen.queryByRole("note")).toBeNull();
    fireEvent.click(within(screen.getByRole("region", { name: "Conversa" })).getByRole("button", { name: "Minimizar" }));
    expect(screen.queryByRole("note")).toBeNull();
  });

  it("já vista em outra sessão: não aparece", async () => {
    setup({ layout: { windows: { plan: win() } }, data: { hints: { seen: ["dock"] } } });
    await settle();
    fireEvent.click(screen.getByRole("button", { name: "Minimizar" }));
    expect(screen.queryByRole("note")).toBeNull();
  });

  it("primeiro arraste: 'Arraste até a borda para encaixar'; a prévia aparece e o encaixe é anunciado", async () => {
    const restoreCapture = installPointerCapturePolyfill();
    const scheduler = manualScheduler();
    try {
      const { status } = setup({ layout: { windows: { conversation: win({ x: 200, y: 100 }) } }, props: { scheduler } });
      await settle();
      const bar = document.getElementById(windowTitleBarId("conversation"));
      if (bar === null) throw new Error("barra ausente");
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerdown", { clientX: 300, clientY: 120 }));
        bar.dispatchEvent(pointerEvent("pointermove", { clientX: 6, clientY: 300 }));
      });
      expect(screen.getByRole("note").textContent).toContain(HINTS.snap);
      const preview = document.querySelector<HTMLElement>("[data-snap-preview]");
      expect(preview?.getAttribute("data-snap-preview")).toBe("left");
      expect(preview?.style.width).toBe(`${WS.w / 2}px`);
      expect(document.querySelector(".nd-root")?.getAttribute("data-gesture")).toBe("true");
      act(() => {
        bar.dispatchEvent(pointerEvent("pointerup", { clientX: 6, clientY: 300 }));
      });
      expect(document.querySelector("[data-snap-preview]")).toBeNull();
      expect(status()).toBe("Conversa encaixada: metade esquerda");
      expect(screen.getByRole("region", { name: "Conversa" }).style.width).toBe(`${WS.w / 2}px`);
    } finally {
      restoreCapture();
    }
  });
});

describe("Desktop: ícones, nunca caracteres", () => {
  it("nenhum botão, item de menu ou selo do desktop montado tem caractere proibido", () => {
    setup({
      layout: { windows: { conversation: win(), plan: win({ z: 2 }) } },
      props: { windows: { conversation: { subtitle: "Arquiteto · pensando", badge: { count: 3, label: "3 pendências" } } } },
    });
    fireEvent.click(within(screen.getByRole("region", { name: "Plano" })).getByRole("button", { name: "Menu da janela" }));
    fireEvent.click(screen.getByRole("button", { name: "Personalizar" }));
    expect(findForbiddenGlyphs(document.body)).toEqual([]);
  });
});
