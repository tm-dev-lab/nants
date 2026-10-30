/*
 * Janela Conversa montada pelo shell (spec 093, T06): bloqueio por prontidão (`readiness.chat`),
 * 412 `prereq_missing`, o cartão "Plano pronto · Abrir Plano", o plano pronto que abre sem roubar
 * o foco, o pedido do lançador, a rolagem salva no ui-state, os selos dos apps e o arrasto de
 * imagem que não vira gesto do desktop.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent, ProjectState } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { activateApp, appBadge, notReady, READY, renderShell, settle } from "../../desktop/shell-test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const P1 = makeProject("p1", "Loja");
const CHAT = /^\/api\/projects\/p1\/chat$/;

function planning(extra: Partial<ProjectState> = {}): ProjectState {
  return makeProjectState({ id: "p1", name: "Loja", ...extra });
}

function awaiting(): ProjectState {
  return planning({ specs: [makeSpec("s1", "Loja online", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Carrinho", "TODO")])] });
}

function conversation(): HTMLElement {
  return screen.getByRole("region", { name: "Conversa" });
}

function field(): HTMLTextAreaElement {
  return within(conversation()).getByRole("textbox", { name: "Mensagem para o Arquiteto" }) as HTMLTextAreaElement;
}


describe("Conversa: prontidão para conversar", () => {
  it("sem readiness.chat: campo, anexar e enviar desabilitados, com o que falta e Configurar na aba certa", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() }, readiness: notReady() });
    const win = conversation();
    expect(field().disabled).toBe(true);
    expect((within(win).getByRole("button", { name: "Anexar imagem" }) as HTMLButtonElement).disabled).toBe(true);
    expect((within(win).getByRole("button", { name: "Enviar" }) as HTMLButtonElement).disabled).toBe(true);
    const notice = within(win).getByRole("region", { name: "Para conversar com o Arquiteto, falta:" });
    expect(notice.textContent).toContain("Conexão com um provedor");
    expect(notice.textContent).toContain("Arquiteto");
    const connection = within(notice).getByRole("button", { name: "Configurar: Conexão com um provedor" });
    expect(connection.getAttribute("data-settings-tab")).toBe("connections");
    expect(within(notice).getByRole("button", { name: "Configurar: Arquiteto" }).getAttribute("data-settings-tab")).toBe("agents");
    fireEvent.click(connection);
    await settle();
    expect(screen.getByRole("region", { name: "Configurações" })).toBeTruthy();
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("com readiness.chat ok, o campo fica livre e sem aviso", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() }, readiness: READY });
    expect(field().disabled).toBe(false);
    expect(document.querySelector("[data-readiness-notice]")).toBeNull();
  });

  it("412 prereq_missing no envio: mensagem do que falta e a prontidão relida (o campo bloqueia)", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() } });
    server.reply("POST", CHAT, () => ({ status: 412, body: { error: "prereq_missing", missing: ["connection"] } }));
    server.model.readiness = notReady();
    fireEvent.change(field(), { target: { value: "quero uma loja" } });
    await act(async () => {
      fireEvent.click(within(conversation()).getByRole("button", { name: "Enviar" }));
    });
    await settle();
    // O rótulo vem da prontidão relida (F-02 da T07), não do texto fixo do cliente.
    expect(within(conversation()).getByText("Falta configurar: Conexão com um provedor.")).toBeTruthy();
    expect(field().disabled).toBe(true);
  });
});

describe("Conversa: plano pronto", () => {
  it("mostra só o cartão compacto 'Plano pronto · Abrir Plano'; o plano fica na janela Plano", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: awaiting() } });
    const card = conversation().querySelector<HTMLElement>("[data-plan-ready]");
    if (card === null) throw new Error("cartão ausente");
    expect(card.textContent).toContain("Plano pronto");
    expect(card.textContent).toContain("2 tarefas");
    expect(within(conversation()).queryByRole("button", { name: /Aprovar e iniciar/ })).toBeNull();
    // Fecha o Plano e reabre pelo cartão.
    const plan = screen.getByRole("region", { name: "Plano" });
    fireEvent.click(within(plan).getByRole("button", { name: "Fechar" }));
    await settle();
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
    fireEvent.click(within(card).getByRole("button", { name: "Abrir Plano" }));
    await settle();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    expect(findForbiddenGlyphs(card)).toEqual([]);
  });

  it("plano que fica pronto abre a janela Plano sem roubar o foco do campo", async () => {
    const { server, stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() } });
    const plan = screen.getByRole("region", { name: "Plano" });
    fireEvent.click(within(plan).getByRole("button", { name: "Fechar" }));
    await settle();
    expect(screen.queryByRole("region", { name: "Plano" })).toBeNull();
    field().focus();
    expect(document.activeElement).toBe(field());
    server.model.states.p1 = awaiting();
    await act(async () => {
      stream().emit("sdd_draft_updated", { event_type: "sdd_draft_updated", project_id: "p1", payload: { project_id: "p1" } } as unknown as ApiEvent);
    });
    await settle();
    expect(screen.getByRole("region", { name: "Plano" })).toBeTruthy();
    expect(document.activeElement).toBe(field());
    // Destaque no menu do N: selo de plano pronto.
    expect(appBadge("plan")).toBe("1");
  });
});

describe("Conversa: pedidos e selos", () => {
  it("'Pedir ao Arquiteto' do lançador vai para o campo, que recebe o foco", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() } });
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    const launcher = screen.getByRole("dialog", { name: "Lançador" });
    fireEvent.change(within(launcher).getByRole("combobox"), { target: { value: "gere os planetas" } });
    await act(async () => {
      fireEvent.click(within(launcher).getByRole("option", { name: /Pedir ao Arquiteto: gere os planetas/ }));
    });
    await settle(5);
    expect(field().value).toBe("gere os planetas");
    expect(document.activeElement).toBe(field());
  });

  it("selo da Conversa no menu do N: pedidos pendentes e mensagens novas com a janela minimizada", async () => {
    const { stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() } });
    expect(appBadge("conversation")).toBeNull();
    // Minimiza a Conversa: mensagens novas ao vivo passam a contar.
    fireEvent.click(within(conversation()).getByRole("button", { name: "Minimizar" }));
    await settle();
    await act(async () => {
      stream().emit("agent_message", {
        event_type: "agent_message",
        project_id: "p1",
        payload: { agent: "architect", text: "Pronto, veja o plano.", turn_id: "t1" },
      });
    });
    await settle();
    expect(appBadge("conversation")).toBe("1");
    // Pedido de permissão pendente também conta.
    await act(async () => {
      stream().emit("permission_requested", {
        event_type: "permission_requested",
        project_id: "p1",
        payload: { request_id: "r1", project_id: "p1", task_id: null, agent: "architect", command: "ls", always_allowed: false },
      });
    });
    await settle();
    expect(appBadge("conversation")).toBe("2");
    // Restaurar a janela zera as novas; o pedido continua.
    activateApp("Conversa");
    await settle();
    expect(appBadge("conversation")).toBe("1");
    expect(appStore.getState().permissions.p1).toHaveLength(1);
  });
});

describe("Conversa: rolagem no ui-state", () => {
  it("restaura a posição salva e grava a nova com atraso, na seção selection", async () => {
    const height = vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockReturnValue(2000);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(300);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: planning() },
      ui: { p1: { rev: 1, data: { selection: { taskId: null, filePath: null, conversationScroll: 500 } } } },
    });
    const box = conversation().querySelector<HTMLElement>("[data-chat-scroll]");
    if (box === null) throw new Error("rolagem ausente");
    expect(box.scrollTop).toBe(500);
    box.scrollTop = 900;
    fireEvent.scroll(box);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 700));
    });
    await settle();
    const put = server.calls.filter((c) => c.method === "PUT" && c.url === "/api/projects/p1/ui-state").pop();
    expect(put?.body).toMatchObject({ section: "selection", value: { conversationScroll: 900 } });
    height.mockRestore();
  });
});

describe("Conversa: arrastar imagem não é gesto do desktop", () => {
  it("arrastar e soltar sobre a janela não move a janela nem liga o modo de gesto", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: planning() } });
    const win = conversation();
    const before = win.getAttribute("style");
    const root = document.querySelector<HTMLElement>(".nd-root");
    const zone = win.querySelector<HTMLElement>("[data-conversation]");
    if (zone === null || root === null) throw new Error("conversa ausente");
    const data = { dataTransfer: { types: ["Files"], files: [new File([new Uint8Array([1])], "a.png", { type: "image/png" })], dropEffect: "none" } };
    fireEvent.dragEnter(zone, data);
    expect(root.getAttribute("data-gesture")).not.toBe("true");
    fireEvent.dragOver(zone, data);
    fireEvent.drop(zone, data);
    await settle();
    expect(win.getAttribute("style")).toBe(before);
    expect(root.getAttribute("data-gesture")).not.toBe("true");
    await waitFor(() => expect(within(win).getByRole("list", { name: "Imagens anexadas" })).toBeTruthy());
  });
});
