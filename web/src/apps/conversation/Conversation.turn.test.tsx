/* Turno do Arquiteto na janela Conversa (spec 093, T06). */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { appStore, turnStartClockNow, useAppStore } from "../../store";
import type { ChatMessage } from "../../store";
import { postChat } from "../../api/client";

vi.mock("../../api/client", async (importOriginal) => ({ ...(await importOriginal<typeof import("../../api/client")>()), postChat: vi.fn() }));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

const postChatMock = vi.mocked(postChat);
const NONE: ChatMessage[] = [];

afterEach(() => {
  cleanup();
  postChatMock.mockReset();
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

function started(id: string, turnId: string, text: string) {
  act(() => {
    appStore.applyEvent({
      event_type: "chat_turn_started",
      project_id: id,
      payload: { project_id: id, turn_id: turnId, user_text: text, started_at: 1 },
    });
  });
}

function ended(id: string, turnId: string) {
  act(() => {
    appStore.applyEvent({
      event_type: "chat_turn_ended",
      project_id: id,
      payload: { project_id: id, turn_id: turnId, ok: true },
    });
  });
}

describe("turno do Arquiteto como estado do servidor", () => {
  it("outra aba mostra a mensagem do usuário e o 'pensando' até o fim do turno", () => {
    const id = "tab-other";
    render(<Harness id={id} />);
    expect(screen.queryByText("O Arquiteto está pensando")).toBeNull();
    started(id, "t1", "ideia vinda do celular");
    expect(screen.getByText("ideia vinda do celular")).not.toBeNull();
    expect(screen.getByText("O Arquiteto está pensando")).not.toBeNull();
    ended(id, "t1");
    expect(screen.queryByText("O Arquiteto está pensando")).toBeNull();
    expect(screen.getByText("ideia vinda do celular")).not.toBeNull();
  });

  it("refresh durante o turno reidrata mensagem e 'pensando' a partir do estado do projeto", () => {
    const id = "tab-refresh";
    appStore.syncArchitectTurn(id, { turn_id: "t2", user_text: "pergunta em andamento", started_at: 5 });
    render(<Harness id={id} />);
    expect(screen.getByText("pergunta em andamento")).not.toBeNull();
    expect(screen.getByText("O Arquiteto está pensando")).not.toBeNull();
    // Reidratar de novo não duplica a mensagem.
    appStore.syncArchitectTurn(id, { turn_id: "t2", user_text: "pergunta em andamento", started_at: 5 });
    expect(screen.getAllByText("pergunta em andamento")).toHaveLength(1);
    act(() => appStore.syncArchitectTurn(id, null));
    expect(screen.queryByText("O Arquiteto está pensando")).toBeNull();
  });

  it("a aba que enviou não duplica a mensagem (dedupe por turn_id)", async () => {
    const id = "tab-sender";
    let resolve: (value: Awaited<ReturnType<typeof postChat>>) => void = () => {};
    postChatMock.mockReturnValue(new Promise((r) => (resolve = r)));
    render(<Harness id={id} />);
    fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "minha ideia" } });
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    started(id, "t3", "minha ideia");
    expect(screen.getAllByText("minha ideia")).toHaveLength(1);
    await act(async () => {
      resolve({ ok: true, data: { reply: "feito", draft_updated: false, turn_id: "t3" } });
    });
    ended(id, "t3");
    expect(screen.getAllByText("minha ideia")).toHaveLength(1);
    expect(screen.getByText("feito")).not.toBeNull();
    expect(screen.queryByText("O Arquiteto está pensando")).toBeNull();
  });

  it("409 por relatório mantém a mensagem com aviso e permite reenviar", async () => {
    const id = "tab-report";
    postChatMock.mockResolvedValueOnce({
      ok: false,
      error: { kind: "http", status: 409, message: "x", reason: "report_in_progress" },
    });
    render(<Harness id={id} />);
    fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "durante o relatório" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    });
    expect(screen.getByText("durante o relatório")).not.toBeNull();
    expect(screen.getByRole("alert").textContent).toContain("O Arquiteto está terminando o relatório; tente em instantes");
    postChatMock.mockResolvedValueOnce({ ok: true, data: { reply: "ok agora", draft_updated: false, turn_id: "t4" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reenviar" }));
    });
    expect(postChatMock).toHaveBeenCalledTimes(2);
    expect(postChatMock.mock.calls[1]?.[1]).toBe("durante o relatório");
    expect(screen.queryByRole("button", { name: "Reenviar" })).toBeNull();
    expect(screen.getAllByText("durante o relatório")).toHaveLength(1);
    expect(screen.getByText("ok agora")).not.toBeNull();
  });

  it("snapshot atrasado não reabre turno encerrado nem apaga turno novo", () => {
    const id = "tab-race";
    const clock = turnStartClockNow();
    started(id, "r1", "um");
    ended(id, "r1");
    // GET emitido com o turno ativo resolve depois do ended: ignorado.
    appStore.syncArchitectTurn(id, { turn_id: "r1", user_text: "um", started_at: 1 }, clock);
    expect(appStore.getState().architectTurns[id]).toBeUndefined();
    // null atrasado (GET anterior a um started) não apaga o turno novo.
    const clock2 = turnStartClockNow();
    started(id, "r2", "dois");
    appStore.syncArchitectTurn(id, null, clock2);
    expect(appStore.getState().architectTurns[id]?.turnId).toBe("r2");
  });

  it("POST falho não captura o turn_id de outro turno", async () => {
    const id = "tab-failed-post";
    postChatMock.mockResolvedValueOnce({ ok: false, error: { kind: "http", status: 502, message: "falha" } });
    render(<Harness id={id} />);
    fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "enviada e falhou" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    });
    started(id, "f1", "mensagem de outra aba");
    expect(screen.getByText("mensagem de outra aba")).not.toBeNull();
    expect(screen.getByText("enviada e falhou")).not.toBeNull();
  });
});
