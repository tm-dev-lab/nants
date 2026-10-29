/*
 * Modo celular no shell (spec 093, T10; decisão 17): troca de modo sem remontar as janelas, abas
 * por fase, Alternador em lista, barra compacta, teclado virtual e a seção `mobile` isolada (o
 * layout e a seleção do desktop não mudam).
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { createElement, memo } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_APPS, type AppRegistry } from "../apps/registry";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../test-utils";
import { findForbiddenGlyphs } from "./glyphs";
import { useSpace } from "./shellContext";
import { notReady, renderShell, settle, type ServerCall } from "./shell-test-utils";
import { MOBILE_QUERY } from "./viewport";

const P1 = makeProject("p1", "Loja");

/** `matchMedia` falso: só o `MOBILE_QUERY` responde; `set` simula girar o aparelho. */
function installMatchMedia(initial: boolean) {
  let mobile = initial;
  const listeners = new Set<() => void>();
  const original = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    media: query,
    get matches() {
      return query === MOBILE_QUERY ? mobile : false;
    },
    addEventListener: (_type: string, listener: () => void) => {
      if (query === MOBILE_QUERY) listeners.add(listener);
    },
    removeEventListener: (_type: string, listener: () => void) => {
      listeners.delete(listener);
    },
  })) as unknown as typeof window.matchMedia;
  return {
    async set(next: boolean): Promise<void> {
      mobile = next;
      await act(async () => {
        listeners.forEach((listener) => listener());
      });
      await settle();
    },
    restore(): void {
      if (original === undefined) Reflect.deleteProperty(window, "matchMedia");
      else window.matchMedia = original;
    },
  };
}

let media: ReturnType<typeof installMatchMedia> | null = null;

afterEach(() => {
  cleanup();
  media?.restore();
  media = null;
  vi.unstubAllGlobals();
  vi.useRealTimers();
  delete document.documentElement.dataset.keyboard;
});

function root(): HTMLElement {
  const el = document.querySelector<HTMLElement>(".nd-root");
  if (el === null) throw new Error("desktop ausente");
  return el;
}

function tabs(): HTMLElement {
  return screen.getByRole("navigation", { name: "Abas" });
}

function tabNames(): string[] {
  return within(tabs())
    .getAllByRole("button")
    .map((b) => b.querySelector(".nd-mobile-tab-label")?.textContent ?? "");
}

function windowEl(kind: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-window="${kind}"]`);
  if (el === null) throw new Error(`janela ${kind} ausente`);
  return el;
}

/** Janelas visíveis (não ocultas). */
function visibleWindows(): string[] {
  return Array.from(document.querySelectorAll<HTMLElement>("[data-window]"))
    .filter((el) => !el.hidden)
    .map((el) => el.getAttribute("data-window") ?? "");
}

function uiPuts(calls: readonly ServerCall[], from = 0): { url: string; section: string }[] {
  return calls
    .slice(from)
    .filter((c) => c.method === "PUT" && /ui-state$/.test(c.url))
    .map((c) => ({ url: c.url, section: (c.body as { section: string }).section }));
}

function geometry(el: HTMLElement): string {
  return [el.style.left, el.style.top, el.style.width, el.style.height].join(",");
}

const planning = () => ({ p1: makeProjectState({ id: "p1", name: "Loja" }) });
const executing = () => {
  const spec = makeSpec("s1", "Carrinho", "IN_PROGRESS", [makeTask("T01", "a", "DONE"), makeTask("T02", "b", "IN_PROGRESS")]);
  return { p1: makeProjectState({ id: "p1", name: "Loja", running: true, run: { spec_id: "s1", status: "running" }, specs: [spec] }) };
};
const done = () => ({ p1: makeProjectState({ id: "p1", name: "Loja", specs: [makeSpec("s1", "Blog", "COMPLETED", [makeTask("T01", "a", "DONE")])] }) });

describe("troca de modo (largura < 1024 px)", () => {
  it("desktop → celular → desktop: não remonta as janelas, mantém o rascunho e preserva o layout do desktop", async () => {
    media = installMatchMedia(false);
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: planning() });
    expect(root().hasAttribute("data-mobile")).toBe(false);
    expect(screen.getByRole("navigation", { name: "Dock" })).toBeTruthy();
    const conversation = windowEl("conversation");
    const plan = windowEl("plan");
    const before = { conversation: geometry(conversation), plan: geometry(plan) };
    // O arranjo do desktop já foi salvo (`layouts.planning.current`).
    await settle();
    const savedLayouts = JSON.stringify(server.model.ui.p1?.data.layouts);
    expect(savedLayouts).toContain("conversation");
    const field = within(conversation).getByRole("textbox", { name: /Mensagem/ });
    fireEvent.change(field, { target: { value: "rascunho em andamento" } });
    const mark = server.calls.length;

    await media.set(true);
    expect(root().getAttribute("data-mobile")).toBe("true");
    expect(screen.queryByRole("navigation", { name: "Dock" })).toBeNull();
    expect(tabs()).toBeTruthy();
    // Uma janela por vez: a Conversa (primeira aba do planejamento); o Plano fica oculto.
    expect(visibleWindows()).toEqual(["conversation"]);
    expect(plan.hidden).toBe(true);
    // As mesmas janelas (nenhuma remontagem) e o mesmo rascunho.
    expect(windowEl("conversation")).toBe(conversation);
    expect(windowEl("plan")).toBe(plan);
    expect((within(conversation).getByRole("textbox", { name: /Mensagem/ }) as HTMLTextAreaElement).value).toBe("rascunho em andamento");
    // Sem redimensionar nem encaixar: nenhuma alça e só Fechar na barra de título.
    expect(conversation.querySelector("[data-handle]")).toBeNull();
    const titleButtons = within(conversation.querySelector<HTMLElement>("[data-window-titlebar]") ?? conversation).getAllByRole("button");
    expect(titleButtons.map((b) => b.getAttribute("aria-label"))).toEqual(["Fechar"]);
    fireEvent.doubleClick(conversation.querySelector("[data-window-titlebar]") ?? conversation);
    expect(conversation.getAttribute("data-mode")).toBe("maximized");
    expect(findForbiddenGlyphs(root())).toEqual([]);

    // No celular, trocar de aba grava só a seção `mobile`.
    fireEvent.click(within(tabs()).getByRole("button", { name: "Plano" }));
    await settle();
    expect(visibleWindows()).toEqual(["plan"]);

    await media.set(false);
    expect(root().hasAttribute("data-mobile")).toBe(false);
    expect(screen.getByRole("navigation", { name: "Dock" })).toBeTruthy();
    expect(windowEl("conversation")).toBe(conversation);
    expect(windowEl("plan")).toBe(plan);
    expect(visibleWindows().sort()).toEqual(["conversation", "plan"]);
    expect({ conversation: geometry(conversation), plan: geometry(plan) }).toEqual(before);
    expect(JSON.stringify(server.model.ui.p1?.data.layouts)).toBe(savedLayouts);
    // Nada além da seção `mobile` foi escrito desde a troca (o layout reaplicado é o salvo).
    const puts = uiPuts(server.calls, mark);
    expect(puts.length).toBeGreaterThan(0);
    expect(puts.every((p) => p.url === "/api/projects/p1/ui-state" && p.section === "mobile")).toBe(true);
    expect((within(conversation).getByRole("textbox", { name: /Mensagem/ }) as HTMLTextAreaElement).value).toBe("rascunho em andamento");
  });

  it("aberto direto no celular e girado para o desktop: o arranjo salvo do desktop é o aplicado", async () => {
    const layout = {
      windows: {
        conversation: { x: 40, y: 30, w: 600, h: 500, z: 2, mode: "normal", open: true },
        files: { x: 300, y: 60, w: 520, h: 400, z: 3, mode: "normal", open: true },
      },
    };
    media = installMatchMedia(true);
    // Tela de 390 px: o arranjo em memória nasce para a tela pequena (e não pode ser salvo); a
    // área de trabalho do desktop, depois de girar, mede 1280 × 680.
    const width = window.innerWidth;
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: 390 });
    const rect = (w: number, h: number): DOMRect => ({ x: 0, y: 0, left: 0, top: 0, right: w, bottom: h, width: w, height: h, toJSON: () => ({}) }) as DOMRect;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      if (!this.hasAttribute("data-workarea")) return rect(0, 0);
      return this.closest("[data-mobile]") !== null ? rect(390, 640) : rect(1280, 680);
    });
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: planning(),
      ui: { p1: { rev: 4, data: { layouts: { planning: { current: layout } } } } },
    });
    Object.defineProperty(window, "innerWidth", { configurable: true, writable: true, value: width });
    expect(root().getAttribute("data-mobile")).toBe("true");
    expect(geometry(windowEl("conversation"))).toBe("0px,0px,390px,640px");
    expect(visibleWindows()).toEqual(["conversation"]);
    expect(uiPuts(server.calls).every((p) => p.section === "mobile")).toBe(true);

    await media.set(false);
    expect(visibleWindows().sort()).toEqual(["conversation", "files"]);
    expect(geometry(windowEl("files"))).toBe("300px,60px,520px,400px");
    expect(server.model.ui.p1?.data.layouts).toEqual({ planning: { current: layout } });
    // Nem de passagem o arranjo da tela pequena foi gravado.
    expect(uiPuts(server.calls).filter((p) => p.section !== "mobile")).toEqual([]);
  });
});

describe("abas por fase", () => {
  it("planejamento: Conversa, Plano, Alternador", async () => {
    media = installMatchMedia(true);
    await renderShell({ url: "/?space=p1", projects: [P1], states: planning() });
    expect(tabNames()).toEqual(["Conversa", "Plano", "Alternador"]);
    expect(within(tabs()).getByRole("button", { name: "Conversa" }).getAttribute("aria-current")).toBe("page");
  });

  it("execução: Conversa, Quadro, Alternador, Atividade", async () => {
    media = installMatchMedia(true);
    await renderShell({ url: "/?space=p1", projects: [P1], states: executing() });
    expect(tabNames()).toEqual(["Conversa", "Quadro", "Alternador", "Atividade"]);
  });

  it("concluído: Relatório, App, Execução, Alternador, Conversa (095/T03, C-12); a primeira aba é a vista inicial", async () => {
    media = installMatchMedia(true);
    await renderShell({ url: "/?space=p1", projects: [P1], states: done() });
    expect(tabNames()).toEqual(["Relatório", "App", "Execução", "Alternador", "Conversa"]);
    expect(visibleWindows()).toEqual(["report"]);
  });

  it("a categoria mudou: as abas trocam e a vista volta à primeira aba", async () => {
    media = installMatchMedia(true);
    const { stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: planning() });
    fireEvent.click(within(tabs()).getByRole("button", { name: "Plano" }));
    await settle();
    expect(visibleWindows()).toEqual(["plan"]);
    await act(async () => {
      stream().emit("project_status", { event_type: "project_status", project_id: "p1", payload: { status: "executing", spec_id: "s1" } });
    });
    await settle();
    expect(tabNames()).toEqual(["Conversa", "Quadro", "Alternador", "Atividade"]);
    expect(visibleWindows()).toEqual(["conversation"]);
  });
});

describe("Alternador", () => {
  it("lista vertical com todas as janelas; tocar abre em tela cheia; Fechar volta ao Alternador", async () => {
    media = installMatchMedia(true);
    await renderShell({ url: "/?space=p1", projects: [P1], states: planning() });
    fireEvent.click(within(tabs()).getByRole("button", { name: "Alternador" }));
    await settle();
    const switcher = screen.getByRole("region", { name: "Janelas" });
    const items = within(switcher).getAllByRole("button");
    expect(items.map((b) => b.querySelector(".nd-switcher-name")?.textContent)).toEqual([
      "Conversa",
      "Plano",
      "Quadro",
      "Atividade",
      "Arquivos",
      "App",
      // 095/T03 (C-12): a Execução está sempre no Alternador (como aba, só no Concluído).
      "Execução",
      "Relatório",
      "Custos",
      "Conhecimento",
      "Projeto",
      "Configurações",
    ]);
    expect(switcher.querySelector("ul")).not.toBeNull();
    // No Alternador nenhuma janela aparece por cima.
    expect(visibleWindows()).toEqual([]);
    expect(findForbiddenGlyphs(switcher)).toEqual([]);

    fireEvent.click(within(switcher).getByRole("button", { name: /Quadro/ }));
    await settle();
    expect(screen.queryByRole("region", { name: "Janelas" })).toBeNull();
    expect(visibleWindows()).toEqual(["board"]);
    expect(windowEl("board").getAttribute("data-mode")).toBe("maximized");
    expect(document.activeElement?.closest("[data-window]")?.getAttribute("data-window")).toBe("board");

    fireEvent.click(within(windowEl("board")).getByRole("button", { name: "Fechar" }));
    await settle();
    expect(screen.getByRole("region", { name: "Janelas" })).toBeTruthy();
    expect(document.activeElement).toBe(within(tabs()).getByRole("button", { name: "Alternador" }));
    // A-12 (094/T08): aberta não é selecionada (o Quadro foi fechado; ver também F-02).
    const back = screen.getByRole("region", { name: "Janelas" });
    expect(back.querySelectorAll('[aria-current="true"]')).toHaveLength(0);
  });

  it("F-02 (094/T08): janela fechada não fica marcada como vista atual no Alternador", async () => {
    media = installMatchMedia(true);
    await renderShell({ url: "/?space=p1", projects: [P1], states: planning() });
    fireEvent.click(within(tabs()).getByRole("button", { name: "Alternador" }));
    await settle();
    fireEvent.click(within(screen.getByRole("region", { name: "Janelas" })).getByRole("button", { name: /Quadro/ }));
    await settle();
    expect(visibleWindows()).toEqual(["board"]);
    // Volta ao Alternador com o Quadro aberto: ele é a vista atual.
    fireEvent.click(within(tabs()).getByRole("button", { name: "Alternador" }));
    await settle();
    expect(Array.from(screen.getByRole("region", { name: "Janelas" }).querySelectorAll('[aria-current="true"]')).map((b) => b.getAttribute("data-kind"))).toEqual(["board"]);
    // Abre de novo e FECHA: no Alternador, nenhum item marcado.
    fireEvent.click(within(screen.getByRole("region", { name: "Janelas" })).getByRole("button", { name: /Quadro/ }));
    await settle();
    fireEvent.click(within(windowEl("board")).getByRole("button", { name: "Fechar" }));
    await settle();
    const switcher = screen.getByRole("region", { name: "Janelas" });
    expect(switcher.querySelectorAll("[aria-current]")).toHaveLength(0);
    expect(within(switcher).getByRole("button", { name: /Quadro/ }).getAttribute("data-open")).toBe("false");
  });

  it("sem espaço: abas Início e Alternador; Configurações abre em tela cheia e Fechar volta ao início", async () => {
    media = installMatchMedia(true);
    await renderShell({ projects: [P1] });
    expect(tabNames()).toEqual(["Início", "Alternador"]);
    expect(screen.getByRole("button", { name: "Novo projeto" })).toBeTruthy();
    fireEvent.click(within(tabs()).getByRole("button", { name: "Alternador" }));
    await settle();
    fireEvent.click(within(screen.getByRole("region", { name: "Janelas" })).getByRole("button", { name: /Configurações/ }));
    await settle();
    expect(visibleWindows()).toEqual(["settings"]);
    fireEvent.click(within(windowEl("settings")).getByRole("button", { name: "Fechar" }));
    await settle();
    expect(visibleWindows()).toEqual([]);
    expect(within(tabs()).getByRole("button", { name: "Início" }).getAttribute("aria-current")).toBe("page");
  });
});

describe("barra compacta e teclado virtual", () => {
  it("só espaço, pílula, busca (só o ícone, C-16), sino e o selo de setup; barra e abas somem com o teclado", async () => {
    media = installMatchMedia(true);
    await renderShell({ url: "/?space=p1", projects: [P1], states: executing(), readiness: notReady() });
    const bar = document.querySelector<HTMLElement>("[data-topbar]");
    if (bar === null) throw new Error("barra ausente");
    expect(bar.getAttribute("data-compact")).toBe("true");
    expect(bar.querySelector("[data-run-pill]")).not.toBeNull();
    expect(bar.querySelector("[data-bell]")).not.toBeNull();
    expect(bar.querySelector("[data-setup-badge]")).not.toBeNull();
    expect(within(bar).getByRole("button", { name: /Loja/ })).toBeTruthy();
    // 095/T02 (C-16): a busca no celular é só o ícone, com o nome acessível.
    const search = bar.querySelector<HTMLElement>("[data-search]");
    expect(search?.getAttribute("data-compact")).toBe("true");
    expect(search?.getAttribute("aria-label")).toBe("Buscar ou pedir");
    expect(search?.textContent).toBe("");
    expect(within(bar).queryByRole("button", { name: /Layout/ })).toBeNull();
    expect(within(bar).queryByText("Automático")).toBeNull();
    expect(screen.queryByRole("button", { name: "Personalizar" })).toBeNull();
    // Teclado: a barra e as abas são `.nd-keyboard-hide`; o campo da Conversa é a área de digitar.
    expect(document.querySelector("header.nd-topbar")?.classList.contains("nd-keyboard-hide")).toBe(true);
    expect(tabs().classList.contains("nd-keyboard-hide")).toBe(true);
    const composer = windowEl("conversation").querySelector("[data-keyboard-area]");
    expect(composer?.querySelector("textarea")).not.toBeNull();
    expect(findForbiddenGlyphs(bar)).toEqual([]);
  });
});

describe("seção `mobile` isolada", () => {
  /** Janela de teste que lê e grava a seleção do espaço. */
  const SelectionProbe = memo(function SelectionProbe() {
    const space = useSpace();
    if (space === null) return null;
    return createElement(
      "div",
      null,
      createElement("output", { "data-probe": "" }, space.selection.taskId ?? "nenhuma"),
      createElement("button", { type: "button", onClick: () => space.setSelection({ taskId: "T09" }) }, "Escolher T09"),
    );
  });
  const apps: AppRegistry = { ...DEFAULT_APPS, conversation: { Content: SelectionProbe, scroll: "app" } };

  it("no celular a seleção vai para `mobile.selection`; a do desktop não muda e volta intacta", async () => {
    media = installMatchMedia(true);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: planning(),
      apps,
      ui: { p1: { rev: 2, data: { selection: { taskId: "T01" } } } },
    });
    expect(document.querySelector("[data-probe]")?.textContent).toBe("T01");
    fireEvent.click(screen.getByRole("button", { name: "Escolher T09" }));
    await settle();
    expect(document.querySelector("[data-probe]")?.textContent).toBe("T09");
    const data = server.model.ui.p1?.data ?? {};
    expect(data.selection).toEqual({ taskId: "T01" });
    expect((data.mobile as { selection?: unknown } | undefined)?.selection).toEqual({ taskId: "T09" });
    expect(uiPuts(server.calls).every((p) => p.url === "/api/projects/p1/ui-state" && p.section === "mobile")).toBe(true);

    await media.set(false);
    expect(document.querySelector("[data-probe]")?.textContent).toBe("T01");
    expect(server.model.ui.p1?.data.selection).toEqual({ taskId: "T01" });
  });
});
