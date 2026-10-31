/*
 * Fronteira de erro por janela (spec 138 T13): um app que lança ao renderizar não deixa a
 * aplicação em branco. A janela dele mostra o aviso com "Recarregar a janela"; a barra superior e
 * as outras janelas continuam. No celular, o mesmo vale para a janela em tela cheia.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DEFAULT_APPS, type AppRegistry } from "../apps/registry";
import { en } from "../i18n/en";
import { ptBR } from "../i18n/ptBR";
import { makeProject, makeProjectState } from "../test-utils";
import { renderShell, settle } from "./shell-test-utils";
import { MOBILE_QUERY } from "./viewport";

const crash = vi.hoisted(() => ({ on: true }));

function Boom() {
  if (crash.on) throw new TypeError("e.acceptance.map is not a function");
  return <p data-boom-ok="">conteúdo de volta</p>;
}

function windowEl(kind: string): HTMLElement {
  const el = document.querySelector<HTMLElement>(`[data-window="${kind}"]`);
  if (el === null) throw new Error(`janela ${kind} ausente`);
  return el;
}

const STATES = { p1: makeProjectState({ id: "p1", name: "Loja" }) };

let originalMatchMedia: typeof window.matchMedia | undefined;

beforeEach(() => {
  crash.on = true;
  originalMatchMedia = window.matchMedia;
  // O React relata o erro capturado no console; o teste não precisa do ruído.
  vi.spyOn(console, "error").mockImplementation(() => undefined);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalMatchMedia === undefined) Reflect.deleteProperty(window, "matchMedia");
  else window.matchMedia = originalMatchMedia;
});

function setMobile(mobile: boolean): void {
  window.matchMedia = ((query: string) => ({
    media: query,
    matches: query === MOBILE_QUERY ? mobile : false,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  })) as unknown as typeof window.matchMedia;
}

describe("Janela com erro de renderização", () => {
  it("só a janela que lançou mostra o aviso; a barra e as outras janelas continuam, e recarregar volta o conteúdo", async () => {
    setMobile(false);
    const apps: AppRegistry = { ...DEFAULT_APPS, plan: { Content: Boom, scroll: "app" } };
    await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: STATES, apps });

    const plan = windowEl("plan");
    const alert = within(plan).getByRole("alert");
    expect(alert.textContent).toContain(ptBR["window.crashed"]);
    // A moldura da janela com erro continua: título e Fechar.
    expect(within(plan).getByRole("button", { name: "Fechar" })).toBeTruthy();
    // A barra superior e a Conversa seguem de pé.
    expect(document.querySelector("[data-topbar]")).not.toBeNull();
    expect(document.querySelector("[data-nbutton]")).not.toBeNull();
    const conversation = windowEl("conversation");
    expect(within(conversation).getByRole("textbox", { name: /Mensagem/ })).toBeTruthy();
    expect(conversation.querySelector("[data-window-crashed]")).toBeNull();

    crash.on = false;
    await act(async () => {
      fireEvent.click(within(plan).getByRole("button", { name: ptBR["window.crashedReload"] }));
    });
    await settle();
    expect(plan.querySelector("[data-boom-ok]")).not.toBeNull();
    expect(plan.querySelector("[data-window-crashed]")).toBeNull();
  });

  it("no celular, a janela em tela cheia com erro mostra o aviso e as abas continuam", async () => {
    setMobile(true);
    const apps: AppRegistry = { ...DEFAULT_APPS, conversation: { Content: Boom, scroll: "app" } };
    await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: STATES, apps });

    expect(document.querySelector(".nd-root")?.getAttribute("data-mobile")).toBe("true");
    const conversation = windowEl("conversation");
    expect(conversation.hidden).toBe(false);
    expect(within(conversation).getByRole("alert").textContent).toContain(ptBR["window.crashed"]);
    expect(screen.getByRole("navigation", { name: "Abas" })).toBeTruthy();
    expect(document.querySelector("[data-topbar]")).not.toBeNull();
  });

  it("o aviso existe nos dois idiomas, sem travessão", () => {
    for (const dict of [ptBR, en]) {
      for (const key of ["window.crashed", "window.crashedReload"] as const) {
        expect(dict[key].length).toBeGreaterThan(0);
        expect(dict[key]).not.toMatch(/[–—]/);
      }
    }
    expect(en["window.crashed"]).not.toBe(ptBR["window.crashed"]);
  });
});
