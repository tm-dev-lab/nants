/*
 * Spec 133 T02: o cartão da pausa por limite de conta (e o da falha do provedor) diz o papel, a
 * conexão e o modelo, e troca ali mesmo a conexão só desse papel: o seletor lista as conexões
 * prontas, "Trocar e continuar" grava a configuração do papel (como a tela de Agentes) e continua.
 */
import { act, cleanup, fireEvent, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { MOBILE_QUERY } from "../../desktop/viewport";
import { setLanguage } from "../../i18n";
import { looksPortuguese } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeConfig, makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

const originalMatchMedia = window.matchMedia;

afterEach(() => {
  cleanup();
  setLanguage("pt-BR", { persist: false });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalMatchMedia === undefined) Reflect.deleteProperty(window, "matchMedia");
  else window.matchMedia = originalMatchMedia;
});

// `makeConfig` põe os três papéis na conexão `conn` (aqui, a Anthropic) com o modelo `model`.
const ANTHROPIC: ConnectionView = { id: "conn", name: "Claude trabalho", base_url: "https://api.anthropic.com", api_key_masked: "****3f9a", kind: "anthropic" };
const PLAN: ConnectionView = {
  id: "plan",
  name: "Plano ChatGPT",
  base_url: "https://chatgpt.com/backend-api/codex",
  api_key_masked: "",
  kind: "chatgpt_plan",
  chatgpt: { state: "connected", email: "ana@x.com" },
};
const PLAN_OUT: ConnectionView = { ...PLAN, id: "plan-out", name: "Plano sem login", chatgpt: { state: "needs_login", email: "bia@x.com" } };
const NO_KEY: ConnectionView = { id: "gem", name: "Gemini sem chave", base_url: "https://generativelanguage.googleapis.com", api_key_masked: "", kind: "google_gemini" };
const STOPPED = makeSpec("s1", "Votação", "IN_PROGRESS", [makeTask("T03", "Placar", "IN_PROGRESS")]);

const BILLING = {
  provider: "provider_billing",
  role: "developer",
  task: "T03",
  attempt: 1,
  connection_id: "conn",
  connection_name: "Claude trabalho",
  connection_kind: "anthropic",
  model: "model",
  category: "billing",
  status: 402,
  error_type: "billing_error",
};

async function open(connections: ConnectionView[]) {
  const view = await renderShell({ url: "/?space=p1", projects: [makeProject("p1", "Votação")], states: { p1: makeProjectState({ id: "p1", name: "Votação", specs: [STOPPED] }) }, connections });
  view.server.reply("GET", /\/api\/config\/connections\/plan\/models/, () => ({
    status: 200,
    body: { connection_id: "plan", models: [{ id: "gpt-sol" }, { id: "gpt-terra" }], truncated: false },
  }));
  await settle();
  return view;
}

function stopWith(code: string, params: Record<string, string | number>): void {
  act(() => {
    appStore.appendMessage("p1", { role: "agent", agent: "system", specId: "s1", text: "x", code, params });
  });
}

function card(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-paused-card]");
  if (el === null) throw new Error("cartão ausente");
  return el;
}

function selectValue(select: HTMLSelectElement, value: string): void {
  act(() => {
    fireEvent.change(select, { target: { value } });
  });
}

describe("cartão da pausa por cobrança com a troca da conexão do papel", () => {
  it("diz o papel, a conexão e o modelo, lista só as conexões prontas e leva à cobrança do provedor", async () => {
    await open([ANTHROPIC, PLAN, PLAN_OUT, NO_KEY]);
    stopWith("run.billing", BILLING);
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Sem crédito no provedor" })).toBeTruthy();
    expect(article.querySelector(".nd-agent-name")?.textContent).toBe("Desenvolvedor");
    const facts = article.querySelector("[data-role-facts='developer']")?.textContent ?? "";
    for (const part of ["Desenvolvedor", "Claude trabalho", "Anthropic", "model"]) expect(facts).toContain(part);
    expect(within(article).getByRole("heading", { name: "Trocar a conexão do Desenvolvedor" })).toBeTruthy();
    const connection = within(article).getByRole("combobox", { name: /Usar a conexão/ }) as HTMLSelectElement;
    expect([...connection.options].map((o) => o.textContent)).toEqual(["Claude trabalho (Anthropic), a atual", "Plano ChatGPT"]);
    expect(connection.value).toBe("plan");
    const link = within(article).getByRole("link", { name: "Cobrança da conta" });
    expect(link.getAttribute("href")).toBe("https://console.anthropic.com/settings/billing");
    // Nos limites, trocar e continuar é a ação principal; continuar sem trocar é a secundária.
    expect(within(article).getByRole("button", { name: "Trocar e continuar" }).getAttribute("data-variant")).toBe("primary");
    expect(within(article).getByRole("button", { name: "Continuar sem trocar" }).getAttribute("data-variant")).toBeNull();
    expect(findForbiddenGlyphs(article)).toEqual([]);
  });

  it("Trocar e continuar grava só a conexão desse papel e continua com ela", async () => {
    const view = await open([ANTHROPIC, PLAN]);
    view.server.reply("PUT", /\/api\/config\/agents$/, (body) => ({ status: 200, body }));
    view.server.reply("POST", /\/api\/projects\/p1\/specs\/s1\/resume$/, () => ({ status: 202, body: { ok: true, spec_id: "s1" } }));
    stopWith("run.billing", BILLING);
    await settle();
    const article = card();
    const model = within(article).getByRole("combobox", { name: /Modelo/ }) as HTMLSelectElement;
    expect(model.value).toBe("gpt-sol");
    selectValue(model, "gpt-terra");
    await act(async () => {
      fireEvent.click(within(article).getByRole("button", { name: "Trocar e continuar" }));
    });
    await settle();
    const before = makeConfig([]).agents;
    const put = view.server.calls.findIndex((c) => c.method === "PUT" && /\/api\/config\/agents$/.test(c.url));
    const resume = view.server.calls.findIndex((c) => c.method === "POST" && /\/resume$/.test(c.url));
    expect(put).toBeGreaterThanOrEqual(0);
    expect(view.server.calls[put]?.body).toEqual({ ...before, developer: { ...before.developer, connection_id: "plan", model: "gpt-terra" } });
    expect(resume).toBeGreaterThan(put);
    // A configuração da tela passa a ter a nova conexão só no Desenvolvedor.
    const agents = appStore.getState().config?.agents;
    expect(agents?.developer).toMatchObject({ connection_id: "plan", model: "gpt-terra" });
    expect(agents?.architect).toEqual(before.architect);
    expect(agents?.reviewer).toEqual(before.reviewer);
  });

  it("sem outra conexão pronta: o aviso e Adicionar conexão, e Continuar segue disponível", async () => {
    await open([ANTHROPIC, NO_KEY]);
    stopWith("run.billing", BILLING);
    await settle();
    const article = card();
    expect(article.querySelector("[data-no-other-connection]")).not.toBeNull();
    expect(within(article).queryByRole("button", { name: "Trocar e continuar" })).toBeNull();
    expect(within(article).getByRole("button", { name: "Adicionar conexão" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Continuar" })).toBeTruthy();
  });

  it("cota diária do Gemini: o mesmo cartão com o papel e o link de uso do AI Studio", async () => {
    await open([ANTHROPIC, PLAN]);
    stopWith("run.daily_quota", { ...BILLING, provider: "provider_daily_quota", role: "reviewer", connection_id: "gemini", connection_kind: "google_gemini", connection_name: "Gemini pessoal", category: "quota", status: 429 });
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Cota diária esgotada" })).toBeTruthy();
    expect(article.querySelector(".nd-agent-name")?.textContent).toBe("Revisor");
    expect(within(article).getByRole("link", { name: "Limites da conta" }).getAttribute("href")).toBe("https://aistudio.google.com/usage");
    expect(within(article).getByRole("button", { name: "Trocar e continuar" }).getAttribute("data-variant")).toBe("primary");
  });

  it("falha do provedor: Continuar é a ação principal e a troca, a secundária", async () => {
    await open([ANTHROPIC, PLAN]);
    stopWith("run.provider_failed", { ...BILLING, provider: "provider_http_400", category: "bad_request", status: 400, error_type: "invalid_request_error" });
    await settle();
    const article = card();
    expect(article.querySelector(".nd-agent-name")?.textContent).toBe("Desenvolvedor");
    const actions = [...article.querySelectorAll<HTMLElement>(".nd-chat-card-foot button")].map((b) => [b.textContent, b.getAttribute("data-variant")]);
    expect(actions).toEqual([
      ["Continuar", "primary"],
      ["Trocar e continuar", null],
    ]);
    expect(within(article).queryByRole("link")).toBeNull();
  });

  it("em en, sem português", async () => {
    setLanguage("en", { persist: false });
    await open([ANTHROPIC, PLAN]);
    stopWith("run.billing", BILLING);
    await settle();
    const article = card();
    expect(within(article).getByRole("heading", { name: "Out of credit at the provider" })).toBeTruthy();
    expect(within(article).getByRole("heading", { name: "Switch the Developer connection" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Switch and continue" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Continue without switching" })).toBeTruthy();
    expect(within(article).getByRole("link", { name: "Account billing" })).toBeTruthy();
    const text = (article.textContent ?? "").replaceAll("Claude trabalho", "").replaceAll("Plano ChatGPT", "");
    expect(looksPortuguese(text)).toBe(false);
  });

  it("o painel de conexões em uso mostra o papel pausado e troca a conexão só dele", async () => {
    const view = await open([ANTHROPIC, PLAN]);
    view.server.reply("PUT", /\/api\/config\/agents$/, (body) => ({ status: 200, body }));
    stopWith("run.billing", BILLING);
    await settle();
    const footer = document.querySelector<HTMLElement>("[data-role-conn-footer]");
    expect(footer?.textContent).toContain("Desenvolvedor pausado");
    await act(async () => {
      fireEvent.click(within(footer as HTMLElement).getAllByRole("button")[0] as HTMLElement);
    });
    await settle();
    const developer = document.querySelector<HTMLElement>("[data-conn-use='developer']");
    if (developer === null) throw new Error("linha do Desenvolvedor ausente");
    expect(developer.textContent).toContain("Pausado: sem crédito no provedor");
    expect(document.querySelector("[data-conn-use='reviewer']")?.textContent).toContain("Disponível");
    await act(async () => {
      fireEvent.click(within(developer).getByRole("button", { name: "Trocar" }));
    });
    await settle();
    await act(async () => {
      fireEvent.click(within(developer).getByRole("button", { name: "Salvar a troca" }));
    });
    await settle();
    const before = makeConfig([]).agents;
    const put = view.server.calls.find((c) => c.method === "PUT" && /\/api\/config\/agents$/.test(c.url));
    expect(put?.body).toEqual({ ...before, developer: { ...before.developer, connection_id: "plan", model: "gpt-sol" } });
    expect(view.server.count("POST", /\/resume$/)).toBe(0);
  });

  it("com a execução rodando, o painel avisa que a troca vale quando ela recomeçar", async () => {
    const view = await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Votação")],
      states: { p1: makeProjectState({ id: "p1", name: "Votação", specs: [STOPPED], running: true }) },
      connections: [ANTHROPIC, PLAN],
    });
    view.server.reply("GET", /\/api\/config\/connections\/plan\/models/, () => ({ status: 200, body: { connection_id: "plan", models: [{ id: "gpt-sol" }], truncated: false } }));
    view.server.reply("PUT", /\/api\/config\/agents$/, (body) => ({ status: 200, body }));
    await settle();
    const footer = document.querySelector<HTMLElement>("[data-role-conn-footer]");
    await act(async () => {
      fireEvent.click(within(footer as HTMLElement).getAllByRole("button")[0] as HTMLElement);
    });
    await settle();
    const dialog = document.querySelector<HTMLElement>("[data-connections-in-use]");
    expect(dialog?.textContent).toContain("quando a execução partir de novo");
    const developer = document.querySelector<HTMLElement>("[data-conn-use='developer']") as HTMLElement;
    await act(async () => {
      fireEvent.click(within(developer).getByRole("button", { name: "Trocar" }));
    });
    await settle();
    await act(async () => {
      fireEvent.click(within(developer).getByRole("button", { name: "Salvar a troca" }));
    });
    await settle();
    expect(developer.querySelector("[data-saved-running]")?.textContent).toBe("Troca salva. Ela vale quando a execução recomeçar; a que está rodando segue com a conexão de antes.");
  });

  it("no celular, o cartão traz o papel, o seletor e as ações", async () => {
    window.matchMedia = ((query: string) => ({
      media: query,
      matches: query === MOBILE_QUERY,
      addEventListener: () => undefined,
      removeEventListener: () => undefined,
    })) as unknown as typeof window.matchMedia;
    await open([ANTHROPIC, PLAN]);
    stopWith("run.billing", BILLING);
    await settle();
    expect(document.querySelector(".nd-root")?.getAttribute("data-mobile") ?? document.querySelector("[data-mobile='true']")).toBeTruthy();
    const article = card();
    expect(article.querySelector("[data-role-facts='developer']")).not.toBeNull();
    expect(article.querySelector("[data-role-switch='developer']")).not.toBeNull();
    expect(within(article).getByRole("button", { name: "Trocar e continuar" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Continuar sem trocar" })).toBeTruthy();
  });
});
