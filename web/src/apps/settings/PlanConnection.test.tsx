/*
 * Conexão pelo plano ChatGPT na interface (spec 116 T03), montada pelo shell: o passo "Como
 * conectar", o botão "Continue with ChatGPT" (mesma janela; aparelho remoto explica), a linha da
 * conexão (e-mail, selo, estado, Gerenciar uso, Reconectar e Sair com confirmação), a volta do
 * login (Configurações com a conexão pronta, nome editável e modelos; erro traduzido) e as
 * boas-vindas uma vez só.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { notReady, renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { setLanguage } from "../../i18n";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";
import { captureChatgptReturn, PLAN_USAGE_URL, resetChatgptReturn, RETURN_SPACE_KEY } from "./chatgptPlan";
import { authNavigation, storeConnection } from "./PlanConnection";

afterEach(() => {
  cleanup();
  resetChatgptReturn();
  window.sessionStorage.clear();
  setLanguage("pt-BR", { persist: false });
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const KEY: ConnectionView = { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", api_key_masked: "********abcd", kind: "api_key" };
const PLAN: ConnectionView = {
  id: "chatgpt",
  name: "ChatGPT (ana@x.com)",
  base_url: "https://api.openai.com/v1",
  api_key_masked: "",
  wire_api: "responses",
  kind: "chatgpt_plan",
  chatgpt: { state: "connected", email: "ana@x.com" },
};
const AUTH_URL = "https://auth.openai.com/api/accounts/authorize?client_id=dynamic_agent_client";

async function shell(extra: RenderShellOptions = {}) {
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) }, connections: [KEY], ...extra });
  view.server.reply("GET", /^\/api\/config\/connections\/[^/]+\/models/, () => ({
    status: 200,
    body: { models: [{ id: "gpt-5", name: "GPT-5" }, { id: "gpt-5-mini", name: "GPT-5 mini" }], truncated: false },
  }));
  view.server.reply("GET", /^\/api\/system\/prereqs$/, () => ({ status: 200, body: [] }));
  return view;
}

async function openSettings(extra: RenderShellOptions = {}) {
  const view = await shell(extra);
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name: /^(Configurações|Settings)$/ }));
  });
  await settle();
  return view;
}

function settings(): HTMLElement {
  return screen.getByRole("region", { name: /^(Configurações|Settings)$/ });
}

function row(id: string): HTMLElement {
  const el = settings().querySelector<HTMLElement>(`[data-connection="${id}"]`);
  if (el === null) throw new Error(`linha ${id} ausente`);
  return el;
}

describe("Como conectar", () => {
  it("dois cartões; o do plano diz só a frase curta e tem o botão (sem logo); chave de API segue para o formulário", async () => {
    await openSettings();
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Nova conexão" }));
    });
    const step = settings().querySelector<HTMLElement>("[data-connection-method]");
    if (step === null) throw new Error("passo ausente");
    expect(within(step).getByText("Como conectar")).toBeTruthy();
    const plan = step.querySelector<HTMLElement>('[data-method="chatgpt_plan"]');
    expect(plan?.textContent).toBe("Plano ChatGPT (Plus ou Pro)Use o plano da sua conta ChatGPT, sem chave.Continue with ChatGPT");
    expect(plan?.querySelector("img")).toBeNull();
    expect(within(step).getByRole("button", { name: /^Chave de API/ }).textContent).toBe("Chave de APIOpenAI, Azure e outros compatíveis.");
    expect(findForbiddenGlyphs(step)).toEqual([]);
    fireEvent.click(within(step).getByRole("button", { name: /^Chave de API/ }));
    expect(within(settings()).getByLabelText("URL base")).toBeTruthy();
    // "Cancelar" do formulário volta à escolha.
    fireEvent.click(within(settings()).getByRole("button", { name: "Cancelar" }));
    expect(settings().querySelector("[data-connection-method]")).not.toBeNull();
  });

  it("Continue with ChatGPT pede o início e segue para a autorização na mesma janela, guardando o espaço", async () => {
    const { server } = await openSettings();
    const go = vi.spyOn(authNavigation, "go").mockImplementation(() => undefined);
    server.reply("POST", /^\/api\/connections\/chatgpt\/start$/, () => ({ status: 200, body: { url: AUTH_URL, local: true } }));
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Nova conexão" }));
    });
    const button = within(settings()).getByRole("button", { name: "Continue with ChatGPT" });
    fireEvent.click(button);
    fireEvent.click(button);
    await settle();
    expect(server.calls.filter((c) => c.url === "/api/connections/chatgpt/start")).toEqual([{ method: "POST", url: "/api/connections/chatgpt/start", body: {} }]);
    expect(go).toHaveBeenCalledWith(AUTH_URL);
    expect(window.sessionStorage.getItem(RETURN_SPACE_KEY)).toBe("p1");
  });

  it("num aparelho da rede, explica que o login é feito no computador onde o Nants roda", async () => {
    const { server } = await openSettings();
    const go = vi.spyOn(authNavigation, "go").mockImplementation(() => undefined);
    server.reply("POST", /^\/api\/connections\/chatgpt\/start$/, () => ({ status: 200, body: { url: AUTH_URL, local: false } }));
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Nova conexão" }));
    });
    fireEvent.click(within(settings()).getByRole("button", { name: "Continue with ChatGPT" }));
    await settle();
    expect(go).not.toHaveBeenCalled();
    expect(within(settings()).getByRole("status").textContent).toBe(
      "O login pelo plano é feito no navegador do computador onde o Nants roda. Abra o Nants nele para continuar.",
    );
  });

  it("falha do início aparece traduzida pelo código", async () => {
    const { server } = await openSettings();
    server.reply("POST", /^\/api\/connections\/chatgpt\/start$/, () => ({ status: 500, body: { error: "x", code: "chatgpt_store_failed" } }));
    await act(async () => {
      fireEvent.click(within(settings()).getByRole("button", { name: "Nova conexão" }));
    });
    fireEvent.click(within(settings()).getByRole("button", { name: "Continue with ChatGPT" }));
    await settle();
    expect(within(settings()).getByRole("alert").textContent).toBe("Não foi possível começar o login: não foi possível gravar os dados do login pelo plano ChatGPT");
  });

  it("os Primeiros passos têm o mesmo passo", async () => {
    await shell({ connections: [], readiness: notReady() });
    await settle();
    const win = screen.getByRole("region", { name: "Primeiros passos" });
    fireEvent.click(within(win).getByRole("button", { name: /^Conexão/ }));
    expect(within(win).getByText("Como conectar")).toBeTruthy();
    expect(within(win).getByRole("button", { name: "Continue with ChatGPT" })).toBeTruthy();
  });
});

describe("linha da conexão pelo plano", () => {
  it("e-mail, selo e estado; menu com Gerenciar uso, Reconectar e Sair", async () => {
    const { server } = await openSettings({ connections: [KEY, PLAN] });
    const line = row("chatgpt");
    expect(line.querySelector("[data-conn-plan]")?.textContent).toBe("Plano ChatGPTana@x.comconectada");
    expect(line.querySelector(".nd-dot")?.getAttribute("data-tone")).toBeNull();
    // Conectada: nada de botão solto na linha.
    expect(line.querySelector("[data-conn-plan-actions]")).toBeNull();
    const go = vi.spyOn(authNavigation, "go").mockImplementation(() => undefined);
    server.reply("POST", /^\/api\/connections\/chatgpt\/start$/, () => ({ status: 200, body: { url: AUTH_URL, local: true } }));
    await act(async () => {
      fireEvent.click(within(line).getByRole("button", { name: `Mais ações de ${PLAN.name}` }));
    });
    const menu = screen.getByRole("menu", { name: `Ações da conexão ${PLAN.name}` });
    const usage = within(menu).getByRole("menuitem", { name: "Gerenciar uso" });
    expect(usage.getAttribute("href")).toBe(PLAN_USAGE_URL);
    expect(usage.getAttribute("target")).toBe("_blank");
    expect(usage.getAttribute("rel")).toBe("noopener noreferrer");
    await act(async () => {
      fireEvent.click(within(menu).getByRole("menuitem", { name: "Reconectar" }));
    });
    await settle();
    expect(server.calls.find((c) => c.url === "/api/connections/chatgpt/start")?.body).toEqual({ connection_id: "chatgpt" });
    expect(go).toHaveBeenCalledWith(AUTH_URL);
  });

  it("Sair pede confirmação, revoga e a linha passa a pedir o login de novo", async () => {
    const { server } = await openSettings({ connections: [KEY, PLAN] });
    server.reply("POST", /^\/api\/connections\/chatgpt\/chatgpt\/logout$/, () => ({ status: 200, body: { state: "disconnected", revoked: true } }));
    await act(async () => {
      fireEvent.click(within(row("chatgpt")).getByRole("button", { name: `Mais ações de ${PLAN.name}` }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Sair" }));
    });
    const dialog = screen.getByRole("alertdialog", { name: `Sair de ${PLAN.name}?` });
    expect(dialog.textContent).toContain("O Nants revoga o acesso na OpenAI e apaga o login guardado neste computador.");
    // Cancelar não chama nada.
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancelar" }));
    expect(server.count("POST", /logout$/)).toBe(0);
    await act(async () => {
      fireEvent.click(within(row("chatgpt")).getByRole("button", { name: `Mais ações de ${PLAN.name}` }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Sair" }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Sair" }));
    });
    await settle();
    expect(server.count("POST", /^\/api\/connections\/chatgpt\/chatgpt\/logout$/)).toBe(1);
    // Revogação confirmada: fecha sem aviso.
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(document.querySelector("[data-chatgpt-revoke-unconfirmed]")).toBeNull();
    expect(row("chatgpt").querySelector("[data-conn-state]")?.textContent).toBe("sem login");
    expect(within(row("chatgpt")).getByRole("button", { name: `Reconectar ${PLAN.name}` })).toBeTruthy();
  });

  it("revogação não confirmada: o diálogo fica com o aviso, o link e Fechar; a linha vai para sem login", async () => {
    const { server } = await openSettings({ connections: [KEY, PLAN] });
    server.reply("POST", /^\/api\/connections\/chatgpt\/chatgpt\/logout$/, () => ({ status: 200, body: { state: "disconnected", revoked: false } }));
    await act(async () => {
      fireEvent.click(within(row("chatgpt")).getByRole("button", { name: `Mais ações de ${PLAN.name}` }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Sair" }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Sair" }));
    });
    await settle();
    const dialog = screen.getByRole("alertdialog", { name: `Sair de ${PLAN.name}?` });
    const warning = within(dialog).getByRole("alert");
    expect(warning.textContent).toBe(
      "O login foi apagado deste computador, mas a OpenAI não confirmou a revogação do acesso.Para garantir, revise os apps conectados nas configurações da sua conta ChatGPT.",
    );
    expect(within(dialog).getByRole("link", { name: "Abrir as configurações do ChatGPT" }).getAttribute("href")).toBe(PLAN_USAGE_URL);
    expect(within(dialog).queryByRole("button", { name: "Sair" })).toBeNull();
    expect(row("chatgpt").querySelector("[data-conn-state]")?.textContent).toBe("sem login");
    expect(findForbiddenGlyphs(dialog)).toEqual([]);
    // "Fechar" do rodapé (o "x" do cabeçalho tem o mesmo nome).
    const close = within(dialog).getAllByRole("button", { name: "Fechar" }).find((b) => b.textContent === "Fechar");
    if (close === undefined) throw new Error("Fechar ausente");
    fireEvent.click(close);
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("revogação não confirmada em en", async () => {
    setLanguage("en", { persist: false });
    const { server } = await openSettings({ connections: [KEY, PLAN] });
    server.reply("POST", /^\/api\/connections\/chatgpt\/chatgpt\/logout$/, () => ({ status: 200, body: { state: "disconnected", revoked: false } }));
    await act(async () => {
      fireEvent.click(within(row("chatgpt")).getByRole("button", { name: `More actions for ${PLAN.name}` }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Sign out" }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Sign out" }));
    });
    await settle();
    const dialog = screen.getByRole("alertdialog");
    expect(within(dialog).getByRole("alert").textContent).toContain("OpenAI did not confirm that the access was revoked.");
    expect(within(dialog).getByRole("link", { name: "Open ChatGPT settings" })).toBeTruthy();
    expect(within(dialog).getAllByRole("button", { name: "Close" }).some((b) => b.textContent === "Close")).toBe(true);
  });

  it("precisa entrar de novo: ponto âmbar e Reconectar na linha", async () => {
    await openSettings({ connections: [{ ...PLAN, chatgpt: { state: "needs_login", email: "ana@x.com" } }] });
    const line = row("chatgpt");
    expect(line.querySelector(".nd-dot")?.getAttribute("data-tone")).toBe("amber");
    expect(line.querySelector("[data-conn-state]")?.textContent).toBe("precisa entrar de novo");
    expect(within(line).getByRole("button", { name: `Reconectar ${PLAN.name}` }).textContent).toBe("Reconectar");
  });

  it("Editar mostra o nome (editável) e os modelos da conta; salvar manda só o nome", async () => {
    const { server } = await openSettings({ connections: [KEY, PLAN] });
    server.reply("PUT", /^\/api\/config\/connections$/, (body) => {
      const b = body as { id: string; name: string };
      return { status: 200, body: { ...PLAN, name: b.name, chatgpt: undefined } };
    });
    await act(async () => {
      fireEvent.click(within(row("chatgpt")).getByRole("button", { name: `Mais ações de ${PLAN.name}` }));
    });
    await act(async () => {
      fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Editar" }));
    });
    await settle();
    const panel = within(settings()).getByRole("region", { name: `Editar ${PLAN.name}` });
    expect(within(panel).queryByLabelText("Chave de API")).toBeNull();
    expect(within(panel).getByText("GPT-5 mini")).toBeTruthy();
    fireEvent.change(within(panel).getByLabelText("Nome"), { target: { value: "Plano da Ana" } });
    await act(async () => {
      fireEvent.click(within(panel).getByRole("button", { name: "Concluir" }));
    });
    await settle();
    const put = server.calls.find((c) => c.method === "PUT" && c.url === "/api/config/connections");
    expect(put?.body).toEqual({ id: "chatgpt", name: "Plano da Ana", base_url: "", kind: "chatgpt_plan" });
    // O estado do login continua (a resposta do PUT não o traz).
    expect(appStore.getState().config?.connections.find((c) => c.id === "chatgpt")?.chatgpt).toEqual(PLAN.chatgpt);
    expect(within(settings()).queryByRole("region", { name: `Editar ${PLAN.name}` })).toBeNull();
  });
});

describe("volta do login", () => {
  it("concluído: abre Conexões com a conexão pronta (nome e modelos) e as boas-vindas uma vez", async () => {
    captureChatgptReturn("?chatgpt=connected&connection=chatgpt");
    const { server } = await shell({ connections: [PLAN] });
    await settle();
    const panel = within(settings()).getByRole("region", { name: "Conexão pelo plano pronta" });
    expect((within(panel).getByLabelText("Nome") as HTMLInputElement).value).toBe("ChatGPT (ana@x.com)");
    expect(panel.querySelector("[data-plan-models]")).not.toBeNull();
    const welcome = screen.getByRole("dialog", { name: "Você está usando seu plano ChatGPT" });
    expect(welcome.textContent).toContain("O uso elegível neste app usa o seu plano ChatGPT. Gerencie o uso nas configurações do ChatGPT.");
    expect(within(welcome).getByRole("link", { name: "Gerenciar uso" }).getAttribute("href")).toBe(PLAN_USAGE_URL);
    expect(findForbiddenGlyphs(welcome)).toEqual([]);
    await act(async () => {
      fireEvent.click(within(welcome).getByRole("button", { name: "Entendi" }));
    });
    await settle();
    expect(screen.queryByRole("dialog", { name: "Você está usando seu plano ChatGPT" })).toBeNull();
    expect(server.model.ui.global?.data.chatgpt).toEqual({ welcomed: true });
    await act(async () => {
      fireEvent.click(within(panel).getByRole("button", { name: "Concluir" }));
    });
    expect(within(settings()).queryByRole("region", { name: "Conexão pelo plano pronta" })).toBeNull();
  });

  it("boas-vindas já vistas não voltam", async () => {
    captureChatgptReturn("?chatgpt=connected&connection=chatgpt");
    await shell({ connections: [PLAN], ui: { global: { rev: 1, data: { chatgpt: { welcomed: true } } } } });
    await settle();
    expect(within(settings()).getByRole("region", { name: "Conexão pelo plano pronta" })).toBeTruthy();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("erro: Conexões mostra o motivo traduzido, com Dispensar", async () => {
    setLanguage("en", { persist: false });
    captureChatgptReturn("?chatgpt=error&reason=scope_missing");
    await shell({ connections: [] });
    await settle();
    const win = screen.getByRole("region", { name: "Settings" });
    const alert = within(win).getByRole("alert");
    expect(alert.textContent).toContain("The ChatGPT plan sign-in did not finish. The account did not allow using the plan here. Check that the plan is Plus or Pro.");
    fireEvent.click(within(alert).getByRole("button", { name: "Dismiss" }));
    expect(within(win).queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("store", () => {
  it("conexão nova entra na lista", async () => {
    await shell({ connections: [KEY] });
    act(() => storeConnection(PLAN));
    expect(appStore.getState().config?.connections.map((c) => c.id)).toEqual(["openai", "chatgpt"]);
  });
});
