/*
 * Spec 080/T02: estado ressincronizado sem `architect_turn` encerra o "pensando" e recarrega o
 * histórico do chat (a resposta do turno perdido vem do arquivo persistido).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { refreshProjectState } from "./projectState";
import { getProjectState } from "./api/client";
import { loadChatHistory } from "./chatHistory";
import { appStore } from "./store";
import { makeProjectState, resetStore } from "./test-utils";

vi.mock("./api/client", () => ({ getProjectState: vi.fn() }));
vi.mock("./chatHistory", () => ({ loadChatHistory: vi.fn(async () => {}) }));

const stateMock = vi.mocked(getProjectState);
const historyMock = vi.mocked(loadChatHistory);

beforeEach(() => {
  resetStore();
});

afterEach(() => {
  stateMock.mockReset();
  historyMock.mockClear();
});

describe("refreshProjectState e o turno do Arquiteto", () => {
  it("turno que a tela mostrava e o servidor não tem mais: some e recarrega o histórico", async () => {
    appStore.syncArchitectTurn("p1", { turn_id: "t1", user_text: "plano", started_at: 1 });
    stateMock.mockResolvedValueOnce({ ok: true, data: makeProjectState({ id: "p1", architect_turn: null }) });
    await refreshProjectState("p1");
    expect(appStore.getState().architectTurns.p1).toBeUndefined();
    expect(historyMock).toHaveBeenCalledWith("p1");
  });

  it("sem turno conhecido, o estado sem turno não recarrega o histórico", async () => {
    stateMock.mockResolvedValueOnce({ ok: true, data: makeProjectState({ id: "p1", architect_turn: null }) });
    await refreshProjectState("p1");
    expect(historyMock).not.toHaveBeenCalled();
  });

  it("turno ainda ativo no servidor: mantém e não recarrega", async () => {
    appStore.syncArchitectTurn("p1", { turn_id: "t1", user_text: "plano", started_at: 1 });
    stateMock.mockResolvedValueOnce({
      ok: true,
      data: makeProjectState({ id: "p1", architect_turn: { turn_id: "t1", user_text: "plano", started_at: 1 } }),
    });
    await refreshProjectState("p1");
    expect(appStore.getState().architectTurns.p1?.turnId).toBe("t1");
    expect(historyMock).not.toHaveBeenCalled();
  });
});
