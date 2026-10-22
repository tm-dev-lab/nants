/*
 * Histórico do chat na janela Conversa montada pelo shell: carregado uma vez ao abrir o espaço,
 * em ordem, e sem duplicar nem suprimir respostas pelo texto (dedupe só por turn_id).
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ChatHistoryTurn } from "../../api/types";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { makeProject, makeProjectState } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const PID = "p1";
const TURN_1 = "0123456789abcdef0123456789abcdef";
const TURN_2 = "fedcba9876543210fedcba9876543210";
const CHAT_GET = /^\/api\/projects\/p1\/chat$/;

async function open(turns: ChatHistoryTurn[]) {
  return renderShell({ url: "/?space=p1", projects: [makeProject(PID, "api-pagamentos")], states: { p1: makeProjectState({ id: PID }) }, chat: { p1: turns } });
}

function chatTexts(): string[] {
  return within(screen.getByRole("list", { name: "Mensagens da conversa" }))
    .getAllByRole("listitem")
    .map((li) => li.textContent ?? "");
}

const architect = (text: string, turnId?: string) => ({
  event_type: "agent_message",
  project_id: PID,
  payload: turnId === undefined ? { agent: "architect", text } : { agent: "architect", text, turn_id: turnId },
});

describe("histórico do chat ao abrir o espaço", () => {
  it("recarregar mostra a conversa, em ordem, já no planejamento", async () => {
    await open([
      { role: "user", text: "Quero um login com e-mail", ts: 100 },
      { role: "architect", text: "Qual provedor de e-mail?", ts: 101 },
    ]);
    expect(screen.getByText("Qual provedor de e-mail?")).toBeTruthy();
    expect(chatTexts().join("|")).toMatch(/Quero um login com e-mail.*Qual provedor de e-mail\?/);
  });

  it("carrega o histórico uma vez por abertura, não a cada aviso do fluxo", async () => {
    const { server, stream } = await open([{ role: "user", text: "primeira", ts: 1 }]);
    act(() => stream().emit("task_updated", { event_type: "task_updated", project_id: PID, payload: { spec_id: "s", task_id: "T01", status: "DONE" } }));
    act(() => stream().emit("project_status", { event_type: "project_status", project_id: PID, payload: { status: "idle", reason: "x" } }));
    await settle();
    expect(screen.getByText("primeira")).toBeTruthy();
    expect(server.count("GET", CHAT_GET)).toBe(1);
  });

  it("evento do turno já carregado pelo histórico não duplica a resposta (dedupe por turn_id)", async () => {
    const { stream } = await open([
      { role: "user", text: "Oi", ts: 4, turn_id: TURN_1 },
      { role: "architect", text: "Olá, sou o Arquiteto.", ts: 5, turn_id: TURN_1 },
    ]);
    act(() => stream().emit("agent_message", architect("Olá, sou o Arquiteto.", TURN_1)));
    expect(screen.getAllByText("Olá, sou o Arquiteto.")).toHaveLength(1);
  });

  it("resposta NOVA igual à última do histórico (outro turn_id) aparece: nada é suprimido por texto", async () => {
    const shell = await open([
      { role: "user", text: "Quero X", ts: 1, turn_id: TURN_1 },
      { role: "architect", text: "Entendido.", ts: 2, turn_id: TURN_1 },
    ]);
    shell.server.reply("POST", CHAT_GET, () => ({ status: 200, body: { reply: "Entendido.", draft_updated: false, turn_id: TURN_2 } }));
    const box = screen.getByLabelText("Mensagem para o Arquiteto");
    fireEvent.change(box, { target: { value: "ok" } });
    await act(async () => {
      fireEvent.keyDown(box, { key: "Enter" });
    });
    await waitFor(() => expect(screen.getAllByText("Entendido.")).toHaveLength(2));
  });

  it("evento de um turno NOVO com texto igual ao do histórico sem turn_id não é suprimido", async () => {
    const { stream } = await open([{ role: "architect", text: "Entendido.", ts: 2 }]);
    act(() => stream().emit("agent_message", architect("Entendido.", TURN_2)));
    await waitFor(() => expect(screen.getAllByText("Entendido.")).toHaveLength(2));
  });
});
