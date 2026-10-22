/*
 * Spec 080/T02: turno resiliente na janela Conversa com o acompanhamento do `useSpaceData`. POST do
 * chat derrubado por rede (ou 409 de turno em andamento) vira acompanhamento do turno pelo
 * estado/stream; ao fim, o histórico é recarregado. O "pensando" segue o servidor mesmo com o
 * POST pendurado.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { useSpaceData, useSpaceMessages } from "../../desktop/useSpaceData";
import { appStore } from "../../store";
import { postChat } from "../../api/client";
import { loadChatHistory } from "../../chatHistory";
import { CHAT_LOST_UNSENT_NOTICE, CHAT_TRACKING_NOTICE, CHAT_TURN_BUSY_NOTICE } from "../../copy";
import { installFetch, makeProjectState, resetStore } from "../../test-utils";

vi.mock("../../api/client", async (importOriginal) => ({ ...(await importOriginal<typeof import("../../api/client")>()), postChat: vi.fn() }));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));
vi.mock("../../chatHistory", () => ({ loadChatHistory: vi.fn(async () => {}) }));

const postChatMock = vi.mocked(postChat);
const historyMock = vi.mocked(loadChatHistory);
const THINKING = "O Arquiteto está pensando";

beforeEach(() => {
  resetStore();
  // Atividades do espaço (carga de abertura do useSpaceData).
  installFetch(() => ({ body: [] }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  postChatMock.mockReset();
  historyMock.mockReset();
  historyMock.mockImplementation(async () => {});
});

/** A Conversa como a janela a monta: dados e acompanhamento do turno pelo `useSpaceData`. */
function Harness({ id, onDraftUpdated = async () => {} }: { id: string; onDraftUpdated?: () => Promise<void> }) {
  const space = useSpaceData(id);
  const messages = useSpaceMessages(id);
  return <Conversation projectId={id} messages={messages} tracking={space.tracking} onTrackTurn={space.trackTurn} onDraftUpdated={onDraftUpdated} />;
}

/** Monta e esquece a carga do histórico da abertura do espaço. */
function mount(id: string, onDraftUpdated?: () => Promise<void>) {
  const view = render(<Harness id={id} {...(onDraftUpdated === undefined ? {} : { onDraftUpdated })} />);
  historyMock.mockClear();
  return view;
}

type ChatResult = Awaited<ReturnType<typeof postChat>>;

/** POST controlado pelo teste: devolve o `resolve`. */
function deferredPost(): { resolve: (value: ChatResult) => void } {
  const handle = { resolve: (_value: ChatResult) => {} };
  postChatMock.mockReturnValueOnce(
    new Promise<ChatResult>((r) => {
      handle.resolve = r;
    }),
  );
  return handle;
}

/** Carência do POST após o fim do turno no servidor (Conversa). */
const GRACE_MS = 12_000;

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

async function send(text: string) {
  fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: text } });
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
  });
}

const NETWORK = { ok: false as const, error: { kind: "network" as const, status: 0, message: "sem rede" } };

describe("turno resiliente", () => {
  it("falha de rede vira acompanhamento sem erro; fim do turno recarrega o histórico e remove o aviso", async () => {
    const id = "track-net";
    let resolve: (value: Awaited<ReturnType<typeof postChat>>) => void = () => {};
    postChatMock.mockReturnValue(new Promise((r) => (resolve = r)));
    mount(id);
    await send("monte o plano");
    started(id, "n1", "monte o plano");
    await act(async () => {
      resolve(NETWORK);
    });

    expect(screen.getByText(CHAT_TRACKING_NOTICE)).not.toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByText("sem rede")).toBeNull();
    expect(screen.getAllByText("monte o plano")).toHaveLength(1);
    expect(historyMock).not.toHaveBeenCalled();

    ended(id, "n1");

    await waitFor(() => expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull());
    expect(historyMock).toHaveBeenCalledWith(id);
    expect(screen.queryByText(CHAT_LOST_UNSENT_NOTICE)).toBeNull();
    // A mensagem do usuário foi confirmada com o turno: o histórico a substitui sem duplicar.
    const own = appStore.getState().messages[id]?.find((m) => m.text === "monte o plano");
    expect(own?.confirmed).toBe(true);
    expect(own?.turnId).toBe("n1");
    expect(own?.inFlight).toBe(false);
  });

  it("turno já terminado no servidor: o histórico com a mensagem encerra o acompanhamento sem duplicar", async () => {
    const id = "track-state";
    postChatMock.mockResolvedValueOnce(NETWORK);
    historyMock.mockImplementation(async (pid: string) => {
      appStore.hydrateHistory(pid, [
        { role: "user", text: "pergunta", ts: Math.floor(Date.now() / 1000), turn_id: "h1" },
        { role: "architect", text: "resposta persistida", ts: Math.floor(Date.now() / 1000), turn_id: "h1" },
      ]);
    });
    mount(id);
    await send("pergunta");
    expect(screen.getByText(CHAT_TRACKING_NOTICE)).not.toBeNull();

    act(() => {
      appStore.setState((s) => ({ projectStates: { ...s.projectStates, [id]: makeProjectState({ id }) } }));
    });

    await waitFor(() => expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull());
    expect(historyMock).toHaveBeenCalledWith(id);
    expect(screen.getByText("resposta persistida")).not.toBeNull();
    expect(screen.getAllByText("pergunta")).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "Reenviar" })).toBeNull();
  });

  it("queda antes de o servidor receber: sem turno e sem linha no histórico, a mensagem fica reenviável", async () => {
    const id = "track-lost";
    postChatMock.mockResolvedValueOnce(NETWORK);
    mount(id);
    await send("nunca chegou");
    expect(screen.getByText(CHAT_TRACKING_NOTICE)).not.toBeNull();

    act(() => {
      appStore.setState((s) => ({ projectStates: { ...s.projectStates, [id]: makeProjectState({ id }) } }));
    });

    await waitFor(() => expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull());
    expect(screen.getByText(CHAT_LOST_UNSENT_NOTICE)).not.toBeNull();
    postChatMock.mockResolvedValueOnce({ ok: true, data: { reply: "agora sim", draft_updated: false, turn_id: "l1" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reenviar" }));
    });
    expect(postChatMock.mock.calls[1]?.[1]).toBe("nunca chegou");
    expect(screen.getByText("agora sim")).not.toBeNull();
    expect(screen.getAllByText("nunca chegou")).toHaveLength(1);
  });

  it("timeout e HTTP não viram acompanhamento", async () => {
    const id = "track-http";
    postChatMock.mockResolvedValueOnce({ ok: false, error: { kind: "http", status: 502, message: "falha do provedor" } });
    mount(id);
    await send("oi");
    expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull();
    expect(screen.getByText("falha do provedor")).not.toBeNull();
  });

  it("409 de turno ainda ativo depois da queda mantém o mesmo aviso de acompanhamento", async () => {
    const id = "track-409";
    postChatMock.mockResolvedValueOnce(NETWORK);
    mount(id);
    await send("primeira");
    started(id, "b1", "primeira");
    postChatMock.mockResolvedValueOnce({
      ok: false,
      error: { kind: "http", status: 409, message: "já existe um turno de chat em andamento para este projeto" },
    });
    await send("segunda");
    expect(screen.getByText(CHAT_TRACKING_NOTICE)).not.toBeNull();
    expect(screen.getByText(CHAT_TURN_BUSY_NOTICE)).not.toBeNull();
    expect(screen.queryByText("já existe um turno de chat em andamento para este projeto")).toBeNull();
    ended(id, "b1");
    await waitFor(() => expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull());
    expect(historyMock).toHaveBeenCalledWith(id);
  });

  it("409 sem queda anterior (turno de outro dispositivo) não fala em conexão perdida", async () => {
    const id = "busy-other-device";
    postChatMock.mockResolvedValueOnce({
      ok: false,
      error: { kind: "http", status: 409, message: "já existe um turno de chat em andamento para este projeto" },
    });
    mount(id);
    await send("ao mesmo tempo");
    expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull();
    expect(screen.getByText(CHAT_TURN_BUSY_NOTICE)).not.toBeNull();
    expect(screen.getByRole("button", { name: "Reenviar" })).not.toBeNull();
  });

  it("fim do turno pelo SSE antes da resposta: 502 do POST ainda aparece e nada é abortado", async () => {
    const id = "race-502";
    const post = deferredPost();
    mount(id);
    await send("plano");
    started(id, "r1", "plano");
    ended(id, "r1");
    expect(screen.queryByText(THINKING)).toBeNull();
    const signal = postChatMock.mock.calls[0]?.[2]?.signal;
    await act(async () => {
      post.resolve({ ok: false, error: { kind: "http", status: 502, message: "falha do provedor" } });
    });
    expect(screen.getByText("falha do provedor")).not.toBeNull();
    expect(signal?.aborted).toBe(false);
    expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull();
  });

  it("fim do turno pelo SSE antes da resposta: sucesso com draft_updated chama onDraftUpdated", async () => {
    const id = "race-draft";
    const onDraftUpdated = vi.fn(async () => {});
    const post = deferredPost();
    mount(id, onDraftUpdated);
    await send("monte o plano");
    started(id, "r2", "monte o plano");
    ended(id, "r2");
    const signal = postChatMock.mock.calls[0]?.[2]?.signal;
    await act(async () => {
      post.resolve({ ok: true, data: { reply: "plano pronto", draft_updated: true, turn_id: "r2" } });
    });
    expect(onDraftUpdated).toHaveBeenCalledTimes(1);
    expect(screen.getByText("plano pronto")).not.toBeNull();
    expect(signal?.aborted).toBe(false);
  });

  it("resposta dentro da carência (fake timers): processada e o timer não aborta depois", async () => {
    vi.useFakeTimers();
    try {
      const id = "race-grace";
      const post = deferredPost();
      mount(id);
      await send("oi");
      started(id, "r3", "oi");
      ended(id, "r3");
      await act(async () => {
        await vi.advanceTimersByTimeAsync(GRACE_MS - 1_000);
      });
      const signal = postChatMock.mock.calls[0]?.[2]?.signal;
      expect(signal?.aborted).toBe(false);
      await act(async () => {
        post.resolve({ ok: false, error: { kind: "http", status: 502, message: "falha do provedor" } });
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(GRACE_MS * 2);
      });
      expect(screen.getByText("falha do provedor")).not.toBeNull();
      expect(signal?.aborted).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it("POST pendurado (fake timers): 'pensando' some já; abortado só depois da carência, composer liberado e sem erro tardio", async () => {
    vi.useFakeTimers();
    try {
      const id = "zombie-post";
      const post = deferredPost();
      mount(id);
      await send("ideia");
      expect(screen.getByText(THINKING)).not.toBeNull();
      started(id, "z1", "ideia");
      expect(screen.getByText(THINKING)).not.toBeNull();
      const signal = postChatMock.mock.calls[0]?.[2]?.signal;
      expect(signal?.aborted).toBe(false);

      act(() => appStore.syncArchitectTurn(id, null));
      expect(screen.queryByText(THINKING)).toBeNull();

      // Dentro da carência: o POST segue vivo e o composer ainda está ocupado com ele.
      await act(async () => {
        await vi.advanceTimersByTimeAsync(GRACE_MS - 1);
      });
      expect(signal?.aborted).toBe(false);
      expect(historyMock).not.toHaveBeenCalled();
      fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "outra" } });
      expect((screen.getByRole("button", { name: "Enviar" }) as HTMLButtonElement).disabled).toBe(true);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(1);
      });
      expect(signal?.aborted).toBe(true);
      expect(historyMock).toHaveBeenCalledWith(id);
      const own = appStore.getState().messages[id]?.find((m) => m.text === "ideia");
      expect(own?.inFlight).toBe(false);
      expect(own?.turnId).toBe("z1");
      expect((screen.getByRole("button", { name: "Enviar" }) as HTMLButtonElement).disabled).toBe(false);

      // A falha tardia do POST abortado é ignorada (sem erro na conversa).
      await act(async () => {
        post.resolve({ ok: false, error: { kind: "timeout", status: 0, message: "tempo esgotado" } });
      });
      expect(screen.queryByText("tempo esgotado")).toBeNull();
      expect(screen.queryByRole("alert")).toBeNull();
      expect(screen.queryByText(CHAT_TRACKING_NOTICE)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("desmontar durante a carência cancela o abandono", async () => {
    vi.useFakeTimers();
    try {
      const id = "grace-unmount";
      deferredPost();
      const view = mount(id);
      await send("tchau");
      started(id, "u1", "tchau");
      ended(id, "u1");
      const signal = postChatMock.mock.calls[0]?.[2]?.signal;
      view.unmount();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(GRACE_MS * 2);
      });
      expect(signal?.aborted).toBe(false);
      expect(historyMock).not.toHaveBeenCalled();
    } finally {
      vi.useRealTimers();
    }
  });
});
