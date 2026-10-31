/*
 * Moldura nos dois idiomas (spec 101, T02): barra superior, menu do N, lançador e busca, barras de
 * título e controles, menu da janela, folha de atalhos, Personalizar, aprovação automática,
 * boas-vindas e estados vazios. Em en, nenhuma frase pt-BR conhecida (as do dicionário) nem
 * acento do português fica visível ou nos rótulos acessíveis; em pt-BR, os textos de hoje.
 * Também a troca ao vivo em Configurações, Aparência, e a persistência (servidor e espelho local).
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AppRegistry } from "../apps/registry";
import { DEFAULT_APPS } from "../apps/registry";
import { en } from "../i18n/en";
import { getLanguage, readStoredLanguage, setLanguage, type Lang } from "../i18n";
import { ptBR, type MessageKey } from "../i18n/ptBR";
import { bindLanguageToUiState } from "../i18n/uiStateBinding";
import { makeProject, makeProjectState } from "../test-utils";
import { BRAND_NAME, BRAND_SLOGAN } from "./brand";
import { activateApp, inNMenu, openAllApps, renderShell, settle, TEST_UI_DEPS } from "./shell-test-utils";
import { UI_SECTIONS, UiStateSync } from "./uiState";
import { APP_ORDER } from "./windowMeta";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Shop");
/** Sem apps: cada janela mostra o estado vazio padrão (moldura), sem o conteúdo da 102. */
const FRAME_ONLY: AppRegistry = {};

/**
 * Frases pt-BR conhecidas: os valores do dicionário que mudam em en, cortados nos parâmetros
 * (`{title} aberta` vira "aberta"), com pelo menos 4 letras.
 */
const PT_PHRASES: readonly string[] = (() => {
  const out = new Set<string>();
  for (const key of Object.keys(ptBR) as MessageKey[]) {
    if (ptBR[key] === en[key]) continue;
    for (const part of ptBR[key].split(/\{\w+\}/)) {
      const phrase = part.trim().replace(/^[:,.;]\s*/, "").replace(/[:,.;]$/, "").trim();
      if (phrase.length >= 4) out.add(phrase);
    }
  }
  return [...out];
})();

/** Cada frase como palavra inteira ("Conversa" não casa dentro de "Conversation"). */
const PT_PHRASE_PATTERNS: readonly RegExp[] = PT_PHRASES.map(
  (phrase) => new RegExp(`(?<!\\p{L})${phrase.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?!\\p{L})`, "u"),
);

/** Letras do português que o inglês não usa. */
const PT_LETTERS = /[ãõçáéíóúâêôàÃÕÇÁÉÍÓÚÂÊÔÀ]/;

/** Todo texto visível e acessível do documento (fora dos botões de idioma, cada um escrito no próprio idioma). */
function frameTexts(): string[] {
  const out: string[] = [];
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    if (node.parentElement?.closest("[data-language-choice]") !== null) continue;
    const text = node.textContent?.trim() ?? "";
    if (text !== "") out.push(text);
  }
  for (const el of document.body.querySelectorAll("*")) {
    if (el.closest("[data-language-choice]") !== null) continue;
    for (const attr of ["aria-label", "title", "placeholder"]) {
      const value = el.getAttribute(attr);
      if (value !== null && value.trim() !== "") out.push(value.trim());
    }
  }
  return out;
}

/** Textos com frase pt-BR do dicionário ou letra do português (em en, deve ser vazio). */
function portugueseLeftovers(): string[] {
  return frameTexts().filter((text) => {
    if (text === ptBR["language.label"]) return false;
    if (PT_LETTERS.test(text)) return true;
    return PT_PHRASE_PATTERNS.some((pattern) => pattern.test(text));
  });
}


/** Abre todas as janelas pelo menu do N (cada uma mostra o estado vazio padrão). */
async function openAllWindows(): Promise<void> {
  await openAllApps();
}

async function renderSpace(lang: Lang, apps: AppRegistry = FRAME_ONLY) {
  setLanguage(lang, { persist: false });
  return renderShell({
    url: "/?space=p1",
    projects: [P1],
    states: { p1: makeProjectState({ id: "p1", name: "Shop" }) },
    apps,
  });
}

const t = (lang: Lang, key: MessageKey): string => (lang === "en" ? en[key] : ptBR[key]);

/** Percorre a moldura inteira (janelas, menu, lançador, atalhos, Personalizar, aprovação automática). */
async function walkFrame(lang: Lang, check: (where: string) => void): Promise<void> {
  await openAllWindows();
  expect(document.querySelectorAll("[data-window]")).toHaveLength(APP_ORDER.length - 1);
  expect(document.querySelectorAll("[data-empty-state]")).toHaveLength(APP_ORDER.length - 1);
  check("janelas");
  const conversation = screen.getByRole("region", { name: t(lang, "window.conversation") });
  await act(async () => {
    fireEvent.click(within(conversation).getByRole("button", { name: t(lang, "window.menu") }));
  });
  expect(screen.getByRole("menu", { name: lang === "en" ? "Conversation window menu" : "Menu da janela Conversa" })).toBeTruthy();
  check("menu da janela");
  await act(async () => {
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  });
  await act(async () => {
    fireEvent.click(document.querySelector<HTMLElement>("[data-search]") as HTMLElement);
  });
  expect(screen.getByRole("dialog", { name: t(lang, "launcher.label") })).toBeTruthy();
  check("lançador");
  await act(async () => {
    fireEvent.keyDown(screen.getByRole("dialog", { name: t(lang, "launcher.label") }), { key: "Escape" });
  });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: t(lang, "desktop.shortcuts") }));
  });
  expect(screen.getByRole("dialog", { name: t(lang, "desktop.shortcuts") })).toBeTruthy();
  check("atalhos");
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: t(lang, "desktop.personalize") }));
  });
  expect(screen.getByRole("dialog", { name: t(lang, "desktop.personalize") })).toBeTruthy();
  check("Personalizar");
  await act(async () => {
    fireEvent.click(screen.getByRole("switch", { name: t(lang, "autoApprove.label") }));
  });
  expect(screen.getByRole("dialog", { name: t(lang, "autoApprove.confirmTitle") })).toBeTruthy();
  check("aprovação automática");
}

describe("moldura em inglês", () => {
  it("boas-vindas: logo, slogan e a linha de produto em inglês, sem frase pt-BR", async () => {
    setLanguage("en", { persist: false });
    await renderShell({ projects: [P1] });
    const welcome = document.querySelector<HTMLElement>("[data-welcome]");
    if (welcome === null) throw new Error("boas-vindas ausentes");
    const logo = welcome.querySelector(".nd-welcome-brand");
    const slogan = within(welcome).getByText(BRAND_SLOGAN);
    const product = within(welcome).getByText("You describe the idea. The agents handle the engineering.");
    expect(logo?.textContent).toBe(BRAND_NAME);
    expect(logo?.nextElementSibling).toBe(slogan);
    expect(slogan.nextElementSibling).toBe(product);
    expect(within(welcome).getByRole("button", { name: "New project" })).toBeTruthy();
    expect(within(welcome).getByRole("button", { name: "Getting started" })).toBeTruthy();
    expect(within(welcome).getByText("Your projects")).toBeTruthy();
    expect(inNMenu((menu) => within(menu).getByRole("button", { name: "Settings" }))).toBeTruthy();
    expect(portugueseLeftovers()).toEqual([]);
  });

  it("espaço aberto: barra, menu do N, títulos, controles, menus e diálogos sem frase pt-BR", async () => {
    await renderSpace("en");
    expect(
      inNMenu((menu) => APP_ORDER.filter((k) => k !== "setup").map((kind) => menu.querySelector(`.nd-nmenu-tile[data-kind="${kind}"]`)?.getAttribute("aria-label"))),
    ).toEqual(["Conversation", "Plan", "Board", "Activity", "Files", "App", "Run", "Report", "Costs", "Knowledge", "Nanty", "Project", "Settings"]);
    expect(screen.getByRole("switch", { name: "Auto-approve" }).textContent).toBe("Auto");
    expect(document.querySelector("[data-search]")?.textContent).toContain("Search or ask");
    await walkFrame("en", (where) => {
      expect([where, portugueseLeftovers()]).toEqual([where, []]);
    });
    expect(screen.getByRole("button", { name: "Turn on auto-approve" })).toBeTruthy();
  });
});

describe("moldura em inglês: erros dos interruptores", () => {
  it("falha ao ligar a aprovação automática: o alerta traz a frase inteira com a mensagem do servidor", async () => {
    const { server } = await renderSpace("en");
    server.reply("PUT", /\/yolo$/, () => ({ status: 422, body: { error: "bad request" } }));
    await act(async () => {
      fireEvent.click(screen.getByRole("switch", { name: "Auto-approve" }));
    });
    const confirm = screen.getByRole("dialog", { name: "Turn on auto-approve?" });
    await act(async () => {
      fireEvent.click(within(confirm).getByRole("button", { name: "Turn on auto-approve" }));
    });
    await settle();
    // A frase é do dicionário; o motivo é a reserva do cliente por status, no idioma ativo (115 T02).
    expect(within(confirm).getByRole("alert").textContent).toBe("Couldn't change auto-approve: invalid data");
  });

  it("falha ao desligar a Internet: o alerta da barra traz a frase inteira com a mensagem do servidor", async () => {
    setLanguage("en", { persist: false });
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: makeProjectState({ id: "p1", name: "Shop", architect_internet: true }) },
      apps: FRAME_ONLY,
    });
    server.reply("PUT", /\/internet$/, () => ({ status: 500, body: { error: "server down" } }));
    await act(async () => {
      fireEvent.click(screen.getByRole("switch", { name: "Architect can use the internet" }));
    });
    await settle();
    expect(document.querySelector("[data-bar-alert='internet'] .nd-bar-alert-text")?.textContent).toBe(
      "Couldn't change the Architect's internet access: internal server error",
    );
  });
});

describe("moldura em pt-BR (igual a hoje)", () => {
  it("espaço aberto: nomes das janelas, barra e diálogos em português", async () => {
    await renderSpace("pt-BR");
    expect(
      inNMenu((menu) => APP_ORDER.filter((k) => k !== "setup").map((kind) => menu.querySelector(`.nd-nmenu-tile[data-kind="${kind}"]`)?.getAttribute("aria-label"))),
    ).toEqual(["Conversa", "Plano", "Quadro", "Atividade", "Arquivos", "App", "Execução", "Relatório", "Custos", "Conhecimento", "Nanty", "Projeto", "Configurações"]);
    expect(screen.getByRole("switch", { name: "Aprovação automática" }).textContent).toBe("Automático");
    expect(document.querySelector("[data-search]")?.textContent).toContain("Buscar ou pedir");
    // O detector do teste em inglês acha as frases de hoje em cada parte da moldura.
    const seen: string[] = [];
    await walkFrame("pt-BR", (where) => {
      expect([where, portugueseLeftovers().length > 0]).toEqual([where, true]);
      seen.push(where);
    });
    expect(seen).toHaveLength(6);
    expect(screen.getByRole("button", { name: "Ligar a aprovação automática" })).toBeTruthy();
  });

  it("boas-vindas com a linha de produto em português", async () => {
    await renderShell({ projects: [P1] });
    const welcome = document.querySelector<HTMLElement>("[data-welcome]");
    if (welcome === null) throw new Error("boas-vindas ausentes");
    expect(within(welcome).getByText("Você descreve a ideia. Os agentes cuidam da engenharia.")).toBeTruthy();
    expect(within(welcome).getByRole("button", { name: "Novo projeto" })).toBeTruthy();
    expect(within(welcome).getByRole("button", { name: "Primeiros passos" })).toBeTruthy();
  });
});

describe("Configurações, Aparência: Idioma / Language", () => {
  async function openAppearance(lang: Lang) {
    const view = await renderSpace(lang, DEFAULT_APPS);
    await act(async () => {
      activateApp(t(lang, "window.settings"));
    });
    await settle();
    const settings = screen.getByRole("region", { name: t(lang, "window.settings") });
    await act(async () => {
      fireEvent.click(within(settings).getByRole("tab", { name: new RegExp(`^${t(lang, "settings.tab.appearance")}`) }));
    });
    await settle();
    return view;
  }

  it("os dois idiomas, sempre escritos no próprio idioma; o ativo fica marcado", async () => {
    await openAppearance("pt-BR");
    const group = screen.getByRole("group", { name: "Idioma / Language" });
    const pt = within(group).getByRole("button", { name: "Português (Brasil)" });
    const english = within(group).getByRole("button", { name: "English" });
    expect(pt.getAttribute("aria-pressed")).toBe("true");
    expect(english.getAttribute("aria-pressed")).toBe("false");
    expect(pt.getAttribute("lang")).toBe("pt-BR");
    expect(english.getAttribute("lang")).toBe("en");
  });

  it("a troca vale na hora (sem recarregar) e fica salva no servidor e no espelho local", async () => {
    const { server, view } = await openAppearance("pt-BR");
    const before = view.container.ownerDocument.body.firstChild;
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "English" }));
    });
    await settle();
    expect(getLanguage()).toBe("en");
    expect(document.documentElement.lang).toBe("en");
    // Ao vivo: a mesma árvore, agora em inglês (menu do N, título da janela, abas, barra).
    expect(view.container.ownerDocument.body.firstChild).toBe(before);
    expect(inNMenu((menu) => within(menu).getByRole("button", { name: "Conversation" }))).toBeTruthy();
    const settings = screen.getByRole("region", { name: "Settings" });
    expect(within(settings).getByRole("tab", { name: /^Appearance/ }).getAttribute("aria-selected")).toBe("true");
    expect(within(settings).getByRole("button", { name: "English" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("switch", { name: "Auto-approve" })).toBeTruthy();
    expect(within(settings).getByText("Theme")).toBeTruthy();
    // Persistida: seção global `language` no servidor e o espelho da primeira pintura.
    await waitFor(() => expect(server.model.ui.global?.data[UI_SECTIONS.language]).toEqual({ lang: "en" }));
    expect(readStoredLanguage()).toBe("en");

    // Volta ao português, também na hora.
    await act(async () => {
      fireEvent.click(within(settings).getByRole("button", { name: "Português (Brasil)" }));
    });
    await settle();
    expect(getLanguage()).toBe("pt-BR");
    expect(inNMenu((menu) => within(menu).getByRole("button", { name: "Conversa" }))).toBeTruthy();
    await waitFor(() => expect(server.model.ui.global?.data[UI_SECTIONS.language]).toEqual({ lang: "pt-BR" }));
  });

  it("depois de recarregar, a preferência salva no servidor volta", async () => {
    const { server } = await openAppearance("pt-BR");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "English" }));
    });
    await waitFor(() => expect(server.model.ui.global?.data[UI_SECTIONS.language]).toEqual({ lang: "en" }));
    cleanup();
    // Recarregar: o navegador abre em português (espelho local em pt-BR) e o servidor vence.
    setLanguage("pt-BR");
    expect(readStoredLanguage()).toBe("pt-BR");
    const sync = new UiStateSync({ kind: "global" }, { ...TEST_UI_DEPS });
    const off = bindLanguageToUiState(sync);
    await sync.start();
    expect(getLanguage()).toBe("en");
    expect(readStoredLanguage()).toBe("en");
    off();
    sync.stop();
  });
});
