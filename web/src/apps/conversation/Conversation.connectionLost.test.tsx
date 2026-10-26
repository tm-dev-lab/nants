/*
 * Queda da conexão no meio do turno do Arquiteto (spec 127 T01): o aviso gravado no histórico
 * aparece na Conversa pelo `code`, no idioma da tela, com "Tentar de novo"; o botão manda a
 * continuação interna pelo servidor (`POST /chat/continue`), sem nada digitado.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { appStore, useAppStore, type ChatMessage } from "../../store";
import { postChat, postChatContinue } from "../../api/client";
import { loadChatHistory } from "../../chatHistory";
import { setLanguage } from "../../i18n";
import { en } from "../../i18n/en";
import { ptBR } from "../../i18n/ptBR";
import type { ChatHistoryTurn } from "../../api/types";

vi.mock("../../api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api/client")>()),
  postChat: vi.fn(),
  postChatContinue: vi.fn(),
}));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));
vi.mock("../../chatHistory", () => ({ loadChatHistory: vi.fn(async () => {}) }));

const postChatMock = vi.mocked(postChat);
const continueMock = vi.mocked(postChatContinue);
const historyMock = vi.mocked(loadChatHistory);
const NONE: ChatMessage[] = [];
const TURN = "0123456789abcdef0123456789abcdef";

afterEach(() => {
  cleanup();
  postChatMock.mockReset();
  continueMock.mockReset();
  historyMock.mockClear();
  setLanguage("pt-BR");
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

function notice(code: string, params: Record<string, string> = {}): ChatHistoryTurn {
  return { role: "system", text: "reserva do servidor", ts: 200, turn_id: TURN, code, params };
}

function hydrate(id: string, turns: ChatHistoryTurn[]) {
  act(() => appStore.hydrateHistory(id, turns));
}

function retryButton(): HTMLButtonElement | null {
  return document.querySelector<HTMLButtonElement>("[data-retry-turn]");
}

describe("Conversa: queda da conexão no meio do turno", () => {
  it("mostra o aviso pelo code, no idioma da tela, com Tentar de novo", () => {
    const id = "lost-pt";
    hydrate(id, [{ role: "user", text: "gera o plano", ts: 100, turn_id: TURN }, notice("chat.connection_lost")]);
    render(<Harness id={id} />);
    expect(screen.getByText(ptBR["system.chat.connection_lost"])).toBeTruthy();
    expect(screen.queryByText("reserva do servidor")).toBeNull();
    expect(retryButton()?.textContent).toContain("Tentar de novo");
    act(() => setLanguage("en"));
    expect(screen.getByText(en["system.chat.connection_lost"])).toBeTruthy();
    expect(retryButton()?.textContent).toContain("Try again");
  });

  it("o aviso com a spec gravada diz o que ficou salvo", () => {
    const id = "lost-saved";
    hydrate(id, [{ role: "user", text: "gera o plano", ts: 100, turn_id: TURN }, notice("chat.connection_lost_saved", { spec: "001-plano" })]);
    render(<Harness id={id} />);
    expect(screen.getByText(ptBR["system.chat.connection_lost_saved"].replace("{spec}", "001-plano"))).toBeTruthy();
  });

  it("Tentar de novo manda a continuação interna e mostra a resposta", async () => {
    const id = "lost-retry";
    hydrate(id, [{ role: "user", text: "gera o plano", ts: 100, turn_id: TURN }, notice("chat.connection_lost")]);
    continueMock.mockResolvedValue({ ok: true, data: { reply: "Tarefas gravadas.", draft_updated: false, turn_id: "fedcba9876543210fedcba9876543210" } });
    render(<Harness id={id} />);
    const button = retryButton();
    if (button === null) throw new Error("sem botão");
    await act(async () => {
      fireEvent.click(button);
    });
    expect(continueMock).toHaveBeenCalledWith(id);
    expect(postChatMock).not.toHaveBeenCalled();
    expect(screen.getByText("Tarefas gravadas.")).toBeTruthy();
    // A resposta é a última mensagem: o botão some.
    expect(retryButton()).toBeNull();
  });

  it("só o aviso que ainda é a última mensagem ganha o botão", () => {
    const id = "lost-old";
    hydrate(id, [
      { role: "user", text: "gera o plano", ts: 100, turn_id: TURN },
      notice("chat.connection_lost"),
      { role: "architect", text: "Plano pronto.", ts: 300 },
    ]);
    render(<Harness id={id} />);
    expect(screen.getByText(ptBR["system.chat.connection_lost"])).toBeTruthy();
    expect(retryButton()).toBeNull();
  });

  it("o 502 com connection_lost recarrega o histórico em vez de repetir o aviso", async () => {
    const id = "lost-post";
    postChatMock.mockResolvedValue({
      ok: false,
      error: { kind: "http", status: 502, message: "x", providerCode: "provider_connect_failed", reason: "connection_lost" },
    });
    render(<Harness id={id} />);
    fireEvent.change(screen.getByLabelText("Mensagem para o Arquiteto"), { target: { value: "gera o plano" } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    });
    expect(historyMock).toHaveBeenCalledWith(id);
    const systems = (appStore.getState().messages[id] ?? []).filter((m) => m.role === "system");
    expect(systems).toHaveLength(0);
  });
});
