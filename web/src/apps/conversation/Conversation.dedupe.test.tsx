/*
 * Resposta do Arquiteto na janela Conversa montada pelo shell: dedupe por `turn_id` entre o POST
 * do chat e o fluxo de eventos, e os avisos do 504 e do POST abortado no cliente.
 */
import { act, cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ChatReply } from "../../api/types";
import { CHAT_CLIENT_TIMEOUT_NOTICE, CHAT_TIMEOUT_DRAFT_SAVED_NOTICE, CHAT_TIMEOUT_NOTICE } from "../../copy";
import { renderShell, settle } from "../../desktop/shell-test-utils";
import { handleLiveEvent } from "../../liveStream";
import { appStore } from "../../store";
import { makeProject, makeProjectState } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const PID = "p1";
const TURN_1 = "0123456789abcdef0123456789abcdef";
const TURN_2 = "fedcba9876543210fedcba9876543210";
const THINKING = "O Arquiteto está pensando";

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

function architectEvent(text: string, turnId?: string) {
  return {
    event_type: "agent_message" as const,
    project_id: PID,
    payload: turnId === undefined ? { agent: "architect", text } : { agent: "architect", text, turn_id: turnId },
  };
}

/** Shell no espaço p1 (planejamento: Conversa + Plano) com o POST do chat controlado pelo teste. */
async function open(chat: (message: string) => Promise<Response>) {
  const shell = await renderShell({ url: "/?space=p1", projects: [makeProject(PID, "api-pagamentos")], states: { p1: makeProjectState({ id: PID }) } });
  const base = shell.fetchMock.getMockImplementation();
  shell.fetchMock.mockImplementation(async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    if (init?.method === "POST" && url === `/api/projects/${PID}/chat`) {
      const body = JSON.parse(String(init.body ?? "{}")) as { message: string };
      return chat(body.message);
    }
    if (base === undefined) throw new Error("fetch ausente");
    return base(input, init);
  });
  await settle();
  return shell;
}

async function send(text: string): Promise<void> {
  const box = screen.getByLabelText("Mensagem para o Arquiteto");
  fireEvent.change(box, { target: { value: text } });
  await act(async () => {
    fireEvent.keyDown(box, { key: "Enter" });
  });
  await settle();
}

describe("resposta do Arquiteto: dedupe por turn_id", () => {
  it("POST abortado no cliente + evento com o turn_id: aviso sem promessa e a resposta aparece uma vez", async () => {
    const { stream } = await open(() => Promise.reject(new DOMException("aborted", "AbortError")));
    await send("oi");
    expect(await screen.findByText(CHAT_CLIENT_TIMEOUT_NOTICE)).toBeTruthy();
    // Sem promessa de recuperação: o fluxo de eventos não tem replay.
    expect(CHAT_CLIENT_TIMEOUT_NOTICE).not.toMatch(/vai aparecer|será exibida|chegará|recuper/);
    act(() => stream().emit("agent_message", architectEvent("Resposta tardia", TURN_1)));
    expect(await screen.findByText("Resposta tardia")).toBeTruthy();
    expect(screen.getAllByText("Resposta tardia")).toHaveLength(1);
  });

  it("504 com draft_saved: avisa que o plano foi salvo", async () => {
    await open(async () => json({ error: "timeout", draft_saved: true, reason: "draft_saved" }, 504));
    await send("oi");
    expect(await screen.findByText(CHAT_TIMEOUT_DRAFT_SAVED_NOTICE)).toBeTruthy();
    expect(screen.queryByText(CHAT_TIMEOUT_NOTICE)).toBeNull();
  });

  it("504 do chat: o servidor cancelou o turno; aviso e nenhuma resposta", async () => {
    await open(async () => json({ error: "timeout" }, 504));
    await send("oi");
    expect(await screen.findByText(CHAT_TIMEOUT_NOTICE)).toBeTruthy();
    expect(CHAT_TIMEOUT_NOTICE).not.toMatch(/publicad|aparece|recuper/);
    expect(screen.queryByText(/Resposta/)).toBeNull();
  });

  it("evento antes do POST: o POST só confirma e a resposta aparece uma vez", async () => {
    let resolveChat: ((r: Response) => void) | undefined;
    const { stream } = await open(() => new Promise<Response>((resolve) => (resolveChat = resolve)));
    await send("oi");
    expect(screen.getByText(THINKING)).toBeTruthy();
    act(() => stream().emit("agent_message", architectEvent("Resposta dupla", TURN_1)));
    expect(await screen.findByText("Resposta dupla")).toBeTruthy();
    const reply: ChatReply = { reply: "Resposta dupla", draft_updated: false, turn_id: TURN_1 };
    await act(async () => resolveChat?.(json(reply)));
    await settle();
    expect(screen.getAllByText("Resposta dupla")).toHaveLength(1);
  });

  it("POST antes do evento: a mesma resposta vinda depois pelo fluxo não duplica", async () => {
    const { stream } = await open(async () => json({ reply: "Resposta única", draft_updated: false, turn_id: TURN_1 }));
    await send("oi");
    expect(await screen.findByText("Resposta única")).toBeTruthy();
    act(() => stream().emit("agent_message", architectEvent("Resposta única", TURN_1)));
    expect(screen.getAllByText("Resposta única")).toHaveLength(1);
  });

  it("dois turnos distintos geram duas mensagens", async () => {
    let turn = 0;
    await open(async () => {
      turn += 1;
      return json({ reply: `Turno ${turn}`, draft_updated: false, turn_id: turn === 1 ? TURN_1 : TURN_2 });
    });
    await send("primeiro");
    expect(await screen.findByText("Turno 1")).toBeTruthy();
    await send("segundo");
    expect(await screen.findByText("Turno 2")).toBeTruthy();
    expect(screen.getAllByText(/^Turno \d$/)).toHaveLength(2);
  });

  it("mensagem do Arquiteto sem turn_id (compatibilidade) aparece sem dedupe", async () => {
    const { stream } = await open(() => new Promise<Response>(() => undefined));
    act(() => stream().emit("agent_message", architectEvent("Sem identidade")));
    act(() => stream().emit("agent_message", architectEvent("Sem identidade")));
    await waitFor(() => expect(screen.getAllByText("Sem identidade")).toHaveLength(2));
    expect(appStore.getState().shownTurns[PID]).toBeUndefined();
  });

  it("o store não guarda mais que o teto de turnos (FIFO)", () => {
    for (let i = 0; i < 205; i += 1) handleLiveEvent(architectEvent(`t${i}`, i.toString(16).padStart(32, "0")));
    const turns = appStore.getState().shownTurns[PID] ?? [];
    expect(turns).toHaveLength(200);
    expect(turns[0]).toBe((5).toString(16).padStart(32, "0"));
  });
});
