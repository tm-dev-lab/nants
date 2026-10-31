/*
 * Janela do Nanty no shell (spec 138 T04, critérios 2, 3 e 6): janela de sistema em qualquer
 * espaço (inclusive sem projeto), no menu do N, no lançador e no Alternador do celular; abas na
 * barra de título; o avatar no estado que vem da API; e o selo sem duplicar o sino.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type { ApiEvent } from "../../api/types";
import { activateApp, appBadge, NANTY_CONFIG, NANTY_EMPTY_FEED, NANTY_STATUS, NANTY_TELEGRAM, openNMenu, renderShell, settle } from "../../desktop/shell-test-utils";
import { MOBILE_QUERY } from "../../desktop/viewport";
import { makeProject, makeProjectState } from "../../test-utils";
import { NANTY_BOT_PHOTO } from "./NantyTelegramCard";

const P1 = makeProject("p1", "Loja");
const SPACE = { url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } };

const waiting = (id: string, group: string, projectId?: string) => ({
  id,
  ...(projectId !== undefined ? { project_id: projectId } : {}),
  group,
  code: `nanty.why.${group.split(":")[1] ?? ""}`,
  params: {},
  created_at_ms: 1,
  expires_at_ms: 9_000_000_000_000,
});

let restoreMedia: (() => void) | null = null;

afterEach(() => {
  cleanup();
  restoreMedia?.();
  restoreMedia = null;
});

function nantyWindow(): HTMLElement {
  const win = document.querySelector<HTMLElement>('[data-window="nanty"]');
  if (win === null) throw new Error("janela do Nanty ausente");
  return win;
}

async function openNanty(): Promise<HTMLElement> {
  await act(async () => activateApp("Nanty"));
  await settle();
  return nantyWindow();
}

describe("janela do Nanty", () => {
  it("no menu do N com o avatar, e abre com as abas Feed, Regras e Ajustes na barra de título", async () => {
    await renderShell({ ...SPACE, nanty: { config: NANTY_CONFIG, telegram: NANTY_TELEGRAM } });
    const tile = openNMenu().querySelector<HTMLElement>('.nd-nmenu-tile[data-kind="nanty"]');
    expect(tile?.getAttribute("aria-label")).toBe("Nanty");
    expect(tile?.querySelector("svg[data-nanty-avatar]")?.getAttribute("data-mood")).toBe("attentive");
    fireEvent.keyDown(window, { key: "Escape" });

    const win = await openNanty();
    const bar = win.querySelector<HTMLElement>("[data-window-titlebar]");
    expect(bar?.querySelector("svg[data-nanty-avatar]")).not.toBeNull();
    // Mesma moldura das outras janelas: os controles reais.
    for (const name of ["Menu da janela", "Minimizar", "Maximizar", "Fechar"]) expect(within(bar as HTMLElement).getByRole("button", { name })).toBeTruthy();
    expect(within(bar as HTMLElement).getByText("atento")).toBeTruthy();
    const tools = bar?.querySelector<HTMLElement>("[data-window-tools]");
    const tabs = within(tools as HTMLElement).getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual(["Feed", "Regras", "Ajustes"]);
    expect(tabs[0]?.getAttribute("aria-selected")).toBe("true");

    const panel = within(win).getByRole("tabpanel");
    expect(panel.classList.contains("nd-app-scroll")).toBe(true);
    expect(panel.querySelector('[data-nanty-empty="feed"]')?.textContent).toContain("Tudo calmo por aqui");
    expect(within(panel).getByRole("img", { name: "Nanty, atento" })).toBeTruthy();

    fireEvent.click(tabs[1] as HTMLElement);
    await settle();
    expect(within(win).getByRole("tabpanel").querySelector("[data-nanty-rules]")?.textContent).toContain("O que o Nanty pode fazer");
    fireEvent.keyDown(tabs[1] as HTMLElement, { key: "ArrowRight" });
    await settle();
    const settings = within(win).getByRole("tabpanel");
    expect(settings.querySelector("[data-nanty-settings]")).not.toBeNull();
    expect(document.activeElement).toBe(tabs[2]);
    // A foto do bot e o passo a passo (o Telegram não aceita foto animada).
    const link = within(settings).getByRole("link", { name: "Baixar a imagem" });
    expect(link.getAttribute("href")).toBe(NANTY_BOT_PHOTO);
    expect(NANTY_BOT_PHOTO).toBe("/brand/nanty-bot.png");
    expect(settings.textContent).toContain("/setuserpic");
    expect(settings.textContent).toContain("não aceita foto animada");
  });

  it("sem projeto (boas-vindas), o Nanty também está no menu do N e abre", async () => {
    await renderShell({ url: "/", projects: [P1] });
    const kinds = Array.from(openNMenu().querySelectorAll(".nd-nmenu-tile")).map((t) => t.getAttribute("data-kind"));
    expect(kinds).toEqual(["nanty", "settings"]);
    fireEvent.keyDown(window, { key: "Escape" });
    const win = await openNanty();
    expect(within(win).getAllByRole("tab")).toHaveLength(3);
  });

  it("no lançador, com o subtítulo vivo", async () => {
    await renderShell(SPACE);
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const launcher = screen.getByRole("dialog", { name: "Lançador" });
    fireEvent.change(within(launcher).getByRole("combobox"), { target: { value: "Nanty" } });
    const item = launcher.querySelector<HTMLElement>('[data-launcher-item="window-nanty"]');
    expect(item?.textContent).toContain("Nanty");
    expect(item?.textContent).toContain("atento");
    await act(async () => {
      fireEvent.click(item as HTMLElement);
    });
    await settle();
    expect(nantyWindow().hidden).toBe(false);
  });

  it("o avatar segue a API: o nanty_changed relê o estado", async () => {
    const view = await renderShell(SPACE);
    const win = await openNanty();
    view.server.reply("GET", /^\/api\/nanty\/status$/, () => ({ status: 200, body: { ...NANTY_STATUS, avatar: "thinking" } }));
    await act(async () => {
      view.stream().emit("nanty_changed", { event_type: "nanty_changed", project_id: null, payload: { part: "avatar" } } as unknown as ApiEvent);
    });
    await settle();
    expect(win.querySelector("[data-window-titlebar] svg[data-nanty-avatar]")?.getAttribute("data-mood")).toBe("thinking");
    expect(within(win.querySelector("[data-window-titlebar]") as HTMLElement).getByText("pensando")).toBeTruthy();
  });
});

describe("selo do Nanty (C-07)", () => {
  it("conta só os cartões que o sino não mostra; o N ganha o ponto", async () => {
    await renderShell({
      ...SPACE,
      attention: {
        setup_incomplete: false,
        projects: [{ project_id: "p1", name: "Loja", pending_permissions: [], pending_choices: false, plan_awaiting_approval: true, paused_or_stalled: null }],
      },
      nanty: {
        status: { ...NANTY_STATUS, avatar: "waiting" },
        feed: { ...NANTY_EMPTY_FEED, waiting: [waiting("a1", "p1:plan_waiting", "p1"), waiting("a2", "*:disk_low"), waiting("a3", "p1:cost_alert", "p1")] },
      },
    });
    // O plano esperando já está no sino: o selo conta o disco e o custo.
    expect(appBadge("nanty")).toBe("2");
    expect(document.querySelector("[data-nbutton-dot]")).not.toBeNull();
    const tile = openNMenu().querySelector<HTMLElement>('.nd-nmenu-tile[data-kind="nanty"]');
    expect(tile?.querySelector("svg[data-nanty-avatar]")?.getAttribute("data-mood")).toBe("waiting");
    expect(tile?.textContent).toContain("2 avisos esperando você");
  });

  it("sem cartão fora do sino, sem selo", async () => {
    await renderShell({
      ...SPACE,
      attention: {
        setup_incomplete: false,
        projects: [{ project_id: "p1", name: "Loja", pending_permissions: [], pending_choices: false, plan_awaiting_approval: true, paused_or_stalled: null }],
      },
      nanty: { feed: { ...NANTY_EMPTY_FEED, waiting: [waiting("a1", "p1:plan_waiting", "p1")] } },
    });
    expect(appBadge("nanty")).toBeNull();
  });
});

describe("celular", () => {
  it("o Nanty está no Alternador, com o avatar e o selo, e abre em tela cheia", async () => {
    const original = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      media: query,
      matches: query === MOBILE_QUERY,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    })) as unknown as typeof window.matchMedia;
    restoreMedia = () => {
      if (original === undefined) Reflect.deleteProperty(window, "matchMedia");
      else window.matchMedia = original;
    };
    await renderShell({ ...SPACE, nanty: { feed: { ...NANTY_EMPTY_FEED, waiting: [waiting("a2", "*:disk_low")] } } });
    fireEvent.click(within(screen.getByRole("navigation", { name: "Abas" })).getByRole("button", { name: "Alternador" }));
    await settle();
    const switcher = screen.getByRole("region", { name: "Janelas" });
    const item = switcher.querySelector<HTMLElement>('.nd-switcher-item[data-kind="nanty"]');
    expect(item?.querySelector("svg[data-nanty-avatar]")).not.toBeNull();
    expect(item?.querySelector("[data-badge]")?.textContent).toBe("1");
    await act(async () => {
      fireEvent.click(item as HTMLElement);
    });
    await settle();
    const win = nantyWindow();
    expect(win.getAttribute("data-mobile")).toBe("true");
    // No celular, as abas ficam na linha logo abaixo da barra.
    expect(win.querySelector('[data-window-tools][data-row="true"] [role="tablist"]')).not.toBeNull();
  });
});
