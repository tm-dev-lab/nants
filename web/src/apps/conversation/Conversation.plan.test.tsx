/*
 * Conversa com o plano ChatGPT (spec 116 T03): o rodapé com a conexão do papel (spec 133, no lugar
 * do indicador "Usando o plano ChatGPT"), com o uso do plano no painel, e o cartão da pausa pelo
 * plano ("Limite de uso atingido" com Gerenciar uso e Continuar; novo login com Reconectar).
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { PLAN_USAGE_URL } from "../settings/chatgptPlan";
import { authNavigation } from "../settings/PlanConnection";
import { ANTHROPIC_LIMITS_URL } from "../../settings/rules";

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const PLAN: ConnectionView = {
  id: "conn",
  name: "ChatGPT (ana@x.com)",
  base_url: "https://api.openai.com/v1",
  api_key_masked: "",
  kind: "chatgpt_plan",
  chatgpt: { state: "connected", email: "ana@x.com" },
};
const KEY: ConnectionView = { id: "conn", name: "OpenAI", base_url: "https://api.openai.com/v1", api_key_masked: "****" };
const STOPPED = makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "A", "IN_PROGRESS")]);

async function open(connections: ConnectionView[], specs = [STOPPED]) {
  // `makeConfig` põe os três papéis na conexão `conn`.
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Loja")], states: { p1: makeProjectState({ id: "p1", name: "Loja", specs }) }, connections });
  await settle();
  return view;
}

function card(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-paused-card]");
  if (el === null) throw new Error("cartão ausente");
  return el;
}

function stopWith(code: string): void {
  act(() => {
    appStore.appendMessage("p1", { role: "agent", agent: "system", specId: "s1", text: "x", code });
  });
}

/** Abre o painel "Conexões em uso" pelo rodapé da Conversa. */
async function openPanel(): Promise<HTMLElement> {
  const footer = document.querySelector<HTMLElement>("[data-role-conn-footer]");
  if (footer === null) throw new Error("rodapé ausente");
  await act(async () => {
    fireEvent.click(within(footer).getByRole("button"));
  });
  await settle();
  return screen.getByRole("dialog");
}

describe("rodapé com a conexão do papel (spec 133, no lugar do indicador do plano)", () => {
  it("diz o papel, o modelo e a conexão; o uso do plano fica no painel", async () => {
    await open([PLAN]);
    const footer = document.querySelector<HTMLElement>("[data-role-conn-footer]");
    expect(footer?.textContent).toContain("Arquiteto");
    expect(footer?.textContent).toContain("model");
    expect(footer?.textContent).toContain("ChatGPT (ana@x.com)");
    expect(footer?.textContent).toContain("Plano ChatGPT");
    expect(footer?.closest(".nd-composer-dock")).not.toBeNull();
    expect(document.querySelector("[data-plan-indicator]")).toBeNull();
    const panel = await openPanel();
    expect(within(panel).getByRole("heading", { name: "Conexões em uso" })).toBeTruthy();
    const architect = panel.querySelector<HTMLElement>("[data-conn-use='architect']");
    expect(within(architect as HTMLElement).getByRole("link", { name: "Gerenciar uso" }).getAttribute("href")).toBe(PLAN_USAGE_URL);
    expect(architect?.textContent).toContain("ana@x.com");
    expect(panel.querySelectorAll("[data-conn-use]")).toHaveLength(3);
    expect(findForbiddenGlyphs(panel)).toEqual([]);
  });

  it("vale para qualquer tipo de conexão, sem o selo do plano", async () => {
    await open([KEY]);
    const footer = document.querySelector<HTMLElement>("[data-role-conn-footer]");
    expect(footer?.textContent).toContain("OpenAI");
    expect(footer?.textContent).toContain("Chave de API");
    const panel = await openPanel();
    expect(within(panel).queryByRole("link", { name: "Gerenciar uso" })).toBeNull();
  });

  it("em en", async () => {
    setLanguage("en", { persist: false });
    await open([PLAN]);
    expect(document.querySelector("[data-role-conn-footer]")?.textContent).toContain("Architect");
    const panel = await openPanel();
    expect(within(panel).getByRole("heading", { name: "Connections in use" })).toBeTruthy();
    expect(within(panel).getAllByRole("link", { name: "Manage usage" }).length).toBeGreaterThan(0);
  });
});

describe("pausa pelo plano", () => {
  it("limite de uso: título próprio, Gerenciar uso e Continuar", async () => {
    await open([PLAN]);
    stopWith("run.plan_limit");
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Limite de uso atingido" })).toBeTruthy();
    expect(article.textContent).toContain("O limite de uso do plano ChatGPT acabou.");
    expect(within(article).getByRole("link", { name: "Gerenciar uso" }).getAttribute("href")).toBe(PLAN_USAGE_URL);
    expect(within(article).getByRole("button", { name: "Continuar" })).toBeTruthy();
    expect(within(article).queryByRole("button", { name: "Reconectar" })).toBeNull();
    expect(findForbiddenGlyphs(article)).toEqual([]);
  });

  it("precisa entrar de novo: Reconectar começa o login da conexão do papel", async () => {
    const { server } = await open([{ ...PLAN, chatgpt: { state: "needs_login", email: "ana@x.com" } }]);
    const go = vi.spyOn(authNavigation, "go").mockImplementation(() => undefined);
    server.reply("POST", /^\/api\/connections\/chatgpt\/start$/, () => ({ status: 200, body: { url: "https://auth.openai.com/x", local: true } }));
    stopWith("run.plan_needs_login");
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Entre de novo no plano ChatGPT" })).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(article).getByRole("button", { name: "Reconectar" }));
    });
    await settle();
    expect(server.calls.find((c) => c.url === "/api/connections/chatgpt/start")?.body).toEqual({ connection_id: "conn" });
    expect(go).toHaveBeenCalledWith("https://auth.openai.com/x");
    expect(within(article).getByRole("button", { name: "Continuar" })).toBeTruthy();
  });

  it("em en", async () => {
    setLanguage("en", { persist: false });
    await open([PLAN]);
    stopWith("run.plan_limit");
    await settle();
    expect(within(card()).getByRole("heading", { name: "Usage limit reached" })).toBeTruthy();
    expect(within(card()).getByRole("link", { name: "Manage usage" })).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Continue" })).toBeTruthy();
  });

  it("outra parada continua com o cartão de sempre", async () => {
    await open([PLAN]);
    stopWith("run.stopped");
    await settle();
    expect(within(card()).getByText("Execução parada")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Limite de uso atingido" })).toBeNull();
  });
});

describe("pausa pelos limites do provedor (spec 132 T09/T10)", () => {
  it("teto de gasto da Anthropic: título, o que fazer, o console e Continuar", async () => {
    await open([KEY]);
    stopWith("run.spend_limit");
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Teto de gasto atingido" })).toBeTruthy();
    expect(article.textContent).toContain("aumente o teto no console do provedor e use Continuar.");
    expect(within(article).getByRole("link", { name: "Abrir os limites da Anthropic" }).getAttribute("href")).toBe(ANTHROPIC_LIMITS_URL);
    expect(within(article).getByRole("button", { name: "Continuar" })).toBeTruthy();
    expect(within(article).queryByRole("link", { name: "Gerenciar uso" })).toBeNull();
    expect(findForbiddenGlyphs(article)).toEqual([]);
  });

  it("cota diária do Gemini: esperar o dia virar ou trocar de conexão, que abre Agentes", async () => {
    await open([KEY]);
    stopWith("run.daily_quota");
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Cota diária esgotada" })).toBeTruthy();
    expect(article.textContent).toContain("use Continuar quando a cota renovar");
    expect(within(article).getByRole("button", { name: "Continuar" })).toBeTruthy();
    await act(async () => {
      fireEvent.click(within(article).getByRole("button", { name: "Trocar de conexão" }));
    });
    await settle();
    const settings = screen.getByRole("region", { name: "Configurações" });
    expect(within(settings).getByRole("tab", { name: /Agentes/, selected: true })).toBeTruthy();
  });

  it("em en", async () => {
    setLanguage("en", { persist: false });
    await open([KEY]);
    stopWith("run.spend_limit");
    await settle();
    expect(within(card()).getByRole("heading", { name: "Spend limit reached" })).toBeTruthy();
    expect(within(card()).getByRole("link", { name: "Open the Anthropic limits" })).toBeTruthy();
    stopWith("run.daily_quota");
    await settle();
    expect(within(card()).getByRole("heading", { name: "Daily quota used up" })).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Switch connection" })).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Continue" })).toBeTruthy();
  });
});
