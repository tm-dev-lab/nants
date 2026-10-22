/* Perguntas do Arquiteto na janela Conversa (spec 093, T06). */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { appStore, useAppStore } from "../../store";
import type { ChatMessage } from "../../store";
import { postChat } from "../../api/client";

vi.mock("../../api/client", async (importOriginal) => ({ ...(await importOriginal<typeof import("../../api/client")>()), postChat: vi.fn() }));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

const postChatMock = vi.mocked(postChat);
const NONE: ChatMessage[] = [];
const BLOCK =
  "Preciso decidir:\n```nants-choices\n" +
  JSON.stringify({ questions: [{ id: "l", text: "Login", multi: false, options: ["Sim", "Não"] }] }) +
  "\n```";

afterEach(() => {
  cleanup();
  postChatMock.mockReset();
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

describe("perguntas do Arquiteto (nants-choices)", () => {
  it("renderiza botões sem o bloco cru e envia pelo caminho do composer", async () => {
    const id = "choices-1";
    postChatMock.mockResolvedValue({ ok: true, data: { reply: "ok", turn_id: "t9", draft_updated: false } } as never);
    act(() => {
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
    });
    render(<Harness id={id} />);
    expect(screen.queryByText(/nants-choices/)).toBeNull();
    fireEvent.click(screen.getByRole("radio", { name: "Sim" }));
    fireEvent.click(screen.getByRole("button", { name: "Enviar respostas" }));
    // 095/T07: as esperas são por condição, com folga para a suíte completa sob carga (o padrão de
    // 1 s do Testing Library estourava de forma intermitente).
    const slow = { timeout: 5_000, interval: 20 };
    await waitFor(() => expect(postChatMock).toHaveBeenCalledWith(id, "Login: Sim", expect.anything()), slow);
    // A resposta do Arquiteto ("ok") é a nova última mensagem: só depois dela o bloco deixa de ser interativo.
    // Esperar por esse efeito (e não pelo desaparecimento do botão) evita a corrida com a renderização.
    await screen.findByText("ok", {}, slow);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Enviar respostas" })).toBeNull(), slow);
    expect(screen.getAllByText("Login: Sim").length).toBeGreaterThan(0);
  });

  it("só a última mensagem do Arquiteto é interativa", () => {
    const id = "choices-2";
    act(() => {
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
    });
    render(<Harness id={id} />);
    expect(screen.getAllByRole("button", { name: "Enviar respostas" })).toHaveLength(1);
  });

  it("mensagem livre do usuário não vira 'Suas escolhas'; resposta do formato e Decida você sim", () => {
    const id = "choices-3";
    act(() => {
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
      appStore.appendMessage(id, { role: "user", text: "Quero um app de notas" });
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
      appStore.appendMessage(id, { role: "user", text: "Login: Sim" });
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
      appStore.appendMessage(id, { role: "user", text: "Siga com as recomendações." });
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
    });
    render(<Harness id={id} />);
    expect(screen.getAllByText("Sem resposta pelas opções")).toHaveLength(1);
    expect(screen.getAllByText("Suas escolhas")).toHaveLength(2);
  });

  it("mensagem marcada fromChoices conta como resposta", () => {
    const id = "choices-4";
    act(() => {
      appStore.appendMessage(id, { role: "architect", text: BLOCK });
      appStore.appendMessage(id, { role: "user", text: "texto qualquer", fromChoices: true });
      appStore.appendMessage(id, { role: "architect", text: "ok" });
    });
    render(<Harness id={id} />);
    expect(screen.getAllByText("Suas escolhas")).toHaveLength(1);
  });
});
