/*
 * Aprovação do plano sem campo de comando de testes (spec 010/T02), na janela Plano montada pelo
 * shell:
 * - nenhum campo ou texto de comando de testes; "Aprovar e iniciar" habilitado com a spec
 *   aguardando aprovação;
 * - spec sem comando (409 test_command_missing): pedido automático e único ao Arquiteto, com o
 *   estado neutro, e as falhas desse pedido com nova tentativa.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState } from "../../api/types";
import { APPROVE_REASON_MESSAGES, ARCHITECT_COMPLETE_PLAN_MESSAGE, ARCHITECT_COMPLETING_NOTICE } from "../../copy";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "api-pagamentos");
const APPROVE = /\/api\/projects\/p1\/specs\/s1\/approve$/;
const CHAT = /^\/api\/projects\/p1\/chat$/;
const STATE = /^\/api\/projects\/p1\/state$/;
const TURN_ID = "0123456789abcdef0123456789abcdef";
const TASKS = [makeTask("T01", "Middleware de autenticação", "TODO")];

function planState(testCommand: string): ProjectState {
  return makeProjectState({ id: "p1", name: "api-pagamentos", test_command: testCommand, specs: [makeSpec("s1", "Autenticação JWT", "AWAITING_APPROVAL", TASKS)] });
}

const plan = (): HTMLElement => screen.getByRole("region", { name: "Plano" });

async function open(state: ProjectState) {
  return renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state } });
}

/** Aprovar e Confirmar início. */
async function approve(): Promise<void> {
  fireEvent.click(within(plan()).getByRole("button", { name: "Aprovar e iniciar" }));
  await act(async () => {
    fireEvent.click(within(plan()).getByRole("button", { name: "Confirmar início" }));
  });
  await settle();
}

/** O POST do chat controlado pelo teste (respostas que o servidor falso não dá). */
function chatWith(shell: Awaited<ReturnType<typeof open>>, reply: (body: unknown) => Promise<Response>) {
  const base = shell.fetchMock.getMockImplementation();
  const bodies: unknown[] = [];
  shell.fetchMock.mockImplementation(async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    if (init?.method === "POST" && CHAT.test(url)) {
      const body: unknown = JSON.parse(String(init.body ?? "{}"));
      bodies.push(body);
      return reply(body);
    }
    if (base === undefined) throw new Error("fetch ausente");
    return base(input, init);
  });
  return bodies;
}

const json = (body: unknown, status = 200): Response => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

describe("Plano: sem campo de comando de testes", () => {
  it("nenhum campo nem texto de comando de testes, mesmo com o comando salvo no projeto; Aprovar habilitado", async () => {
    await open(planState("cargo test"));
    expect(within(plan()).queryByLabelText("Comando de testes")).toBeNull();
    expect(screen.queryByText(/comando de testes/i)).toBeNull();
    expect(screen.queryByText("cargo test")).toBeNull();
    const button = within(plan()).getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
  });

  it("no planejamento (sem plano pronto) também não há campo de comando de testes", async () => {
    await open(makeProjectState({ id: "p1", test_command: "", specs: [makeSpec("s1", "Rascunho", "DRAFT")] }));
    expect(screen.getByLabelText("Mensagem para o Arquiteto")).toBeTruthy();
    expect(screen.queryByLabelText("Comando de testes")).toBeNull();
    expect(screen.queryByText(/comando de testes/i)).toBeNull();
  });

  it("202 normal: aprova sem pedir nada ao Arquiteto e sem mensagem automática", async () => {
    const shell = await open(planState(""));
    shell.server.reply("POST", APPROVE, () => ({ status: 202, body: { ok: true, spec_id: "s1" } }));
    await approve();
    expect(shell.server.count("POST", APPROVE)).toBe(1);
    expect(shell.server.count("POST", CHAT)).toBe(0);
    expect(screen.queryByText(ARCHITECT_COMPLETING_NOTICE)).toBeNull();
    expect(screen.queryByText(ARCHITECT_COMPLETE_PLAN_MESSAGE)).toBeNull();
  });
});

describe("Plano: spec sem comando (409 test_command_missing)", () => {
  async function legacy() {
    const shell = await open(planState(""));
    shell.server.reply("POST", APPROVE, () => ({ status: 409, body: { error: "x", reason: "test_command_missing" } }));
    return shell;
  }

  it("pede ao Arquiteto uma única vez, mostra o estado neutro e volta ao normal com a resposta", async () => {
    const shell = await legacy();
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => (release = resolve));
    const bodies = chatWith(shell, async () => {
      await gate;
      return json({ reply: "Plano atualizado com pytest.", draft_updated: false, turn_id: TURN_ID });
    });
    await approve();
    expect(within(plan()).getByText(ARCHITECT_COMPLETING_NOTICE)).toBeTruthy();
    expect(within(plan()).queryByRole("button", { name: "Aprovar e iniciar" })).toBeNull();
    expect(bodies).toEqual([{ message: ARCHITECT_COMPLETE_PLAN_MESSAGE, internal: true }]);
    await act(async () => release());
    await settle();
    expect(await screen.findByText("Plano atualizado com pytest.")).toBeTruthy();
    expect(bodies).toHaveLength(1);
    expect(screen.queryByText(ARCHITECT_COMPLETE_PLAN_MESSAGE)).toBeNull();
    expect((within(plan()).getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement).disabled).toBe(false);
    expect(within(plan()).queryByText(ARCHITECT_COMPLETING_NOTICE)).toBeNull();
    expect(within(plan()).queryByRole("alert")).toBeNull();
  });

  it("504 do pedido: erro discreto, botão de volta, e o estado do projeto é relido", async () => {
    const shell = await legacy();
    const bodies = chatWith(shell, async () => json({ error: "timeout" }, 504));
    const before = shell.server.count("GET", STATE);
    await approve();
    expect(within(plan()).getByRole("alert").textContent).toContain("O Arquiteto não concluiu a resposta a tempo");
    expect(within(plan()).queryByText(ARCHITECT_COMPLETING_NOTICE)).toBeNull();
    expect((within(plan()).getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement).disabled).toBe(false);
    expect(bodies).toHaveLength(1);
    await waitFor(() => expect(shell.server.count("GET", STATE)).toBeGreaterThan(before));
  });

  it.each([
    ["falha do provedor (502)", async () => json({ error: "provider_connect_failed" }, 502)],
    ["falha de rede real (fetch rejeita)", async (): Promise<Response> => Promise.reject(new TypeError("Failed to fetch"))],
  ])("%s mostra erro e o botão permite tentar de novo", async (_name, reply) => {
    const shell = await legacy();
    const bodies = chatWith(shell, reply);
    await approve();
    expect(within(plan()).getByRole("alert")).toBeTruthy();
    expect(within(plan()).queryByText(ARCHITECT_COMPLETING_NOTICE)).toBeNull();
    await approve();
    expect(bodies).toHaveLength(2);
  });

  it("turno de chat em andamento (409) mostra a mensagem fixa do chat e libera o botão", async () => {
    const shell = await legacy();
    chatWith(shell, async () => json({ error: "já existe um turno de chat em andamento para este projeto" }, 409));
    await approve();
    expect(within(plan()).getByRole("alert").textContent).toContain("já existe um turno de chat em andamento para este projeto");
    expect((within(plan()).getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("depois de recarregar, a conversa não mostra a mensagem interna (o servidor só devolve a resposta)", async () => {
    await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: planState("") },
      chat: { p1: [{ role: "architect", text: "Plano atualizado.", ts: 1, turn_id: TURN_ID }] },
    });
    expect(await screen.findByText("Plano atualizado.")).toBeTruthy();
    expect(screen.queryByText(ARCHITECT_COMPLETE_PLAN_MESSAGE)).toBeNull();
    expect(screen.queryByText(/comando de testes/i)).toBeNull();
  });
});

describe("Plano: falhas da aprovação", () => {
  it.each(Object.entries(APPROVE_REASON_MESSAGES))("409 com reason %s mostra a mensagem humana fixa", async (reason, message) => {
    const shell = await open(planState(""));
    shell.server.reply("POST", APPROVE, () => ({ status: 409, body: { error: "corpo cru", reason } }));
    await approve();
    const alert = within(plan()).getByRole("alert");
    expect(alert.textContent).toContain(message);
    expect(alert.textContent).not.toContain("corpo cru");
  });

  it("422 de agente não configurado oferece Configurar na aba Agentes", async () => {
    const shell = await open(planState(""));
    shell.server.reply("POST", APPROVE, () => ({ status: 422, body: { error: "agente developer não configurado" } }));
    await approve();
    const alert = within(plan()).getByRole("alert");
    expect(within(alert).getByRole("button", { name: "Configurar: Agentes" }).getAttribute("data-settings-tab")).toBe("agents");
  });

  it("422 de outra natureza mostra a mensagem do cliente, sem o link de agentes", async () => {
    const shell = await open(planState(""));
    shell.server.reply("POST", APPROVE, () => ({ status: 422, body: { error: "outro texto qualquer" } }));
    await approve();
    const alert = within(plan()).getByRole("alert");
    expect(alert.textContent).not.toContain("outro texto qualquer");
    expect(within(alert).queryByRole("button", { name: /Configurar/ })).toBeNull();
  });
});
