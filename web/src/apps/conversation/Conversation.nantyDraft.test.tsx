/*
 * Rascunho do Nanty na caixa do Arquiteto (spec 140 T15, C-15): o texto entra na caixa vazia com a
 * faixa que diz de onde veio (e a marca de dado não confiável); com texto da pessoa, nada é trocado sem
 * ela pedir; nada é enviado sozinho, e o envio dela leva o id do rascunho para o servidor decidir a
 * internet do turno.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { bumpArchitectDraft } from "./architectDraft";
import { useAppStore, type ChatMessage } from "../../store";
import { discardArchitectDraft, getArchitectDraft, postChat, type ArchitectDraft } from "../../api/client";
import { setLanguage } from "../../i18n";
import { en } from "../../i18n/en";
import { ptBR } from "../../i18n/ptBR";

vi.mock("../../api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api/client")>()),
  postChat: vi.fn(),
  getArchitectDraft: vi.fn(),
  discardArchitectDraft: vi.fn(async () => ({ ok: true, data: { discarded: true } })),
}));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

const postChatMock = vi.mocked(postChat);
const getMock = vi.mocked(getArchitectDraft);
const discardMock = vi.mocked(discardArchitectDraft);
const NONE: ChatMessage[] = [];
const DRAFT_ID = "0123456789abcdef";

afterEach(() => {
  cleanup();
  postChatMock.mockReset();
  getMock.mockReset();
  discardMock.mockClear();
  setLanguage("pt-BR");
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

function serve(draft: ArchitectDraft | null) {
  getMock.mockResolvedValue({ ok: true, data: draft });
}

function box(): HTMLTextAreaElement {
  return screen.getByLabelText(ptBR["conversation.inputLabel"]) as HTMLTextAreaElement;
}

describe("rascunho do Nanty na caixa do Arquiteto", () => {
  it("entra na caixa vazia com a marca e só o envio da pessoa o leva, com o id", async () => {
    const id = "draft-marked";
    serve({ id: DRAFT_ID, text: "Separar o checkout em duas etapas.", marked: true });
    postChatMock.mockResolvedValue({ ok: true, data: { reply: "Vamos ver.", draft_updated: false, turn_id: "fedcba9876543210fedcba9876543210" } });
    render(<Harness id={id} />);
    await waitFor(() => expect(box().value).toBe("Separar o checkout em duas etapas."));
    expect(screen.getByText(ptBR["conversation.nantyDraft.title"])).toBeTruthy();
    expect(screen.getByText(ptBR["conversation.nantyDraft.marked"])).toBeTruthy();
    expect(document.querySelector("[data-nanty-draft='marked']")).not.toBeNull();
    expect(postChatMock).not.toHaveBeenCalled();
    fireEvent.change(box(), { target: { value: "Separar o checkout em duas etapas. Revisado." } });
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: ptBR["conversation.send"] }));
    });
    expect(postChatMock).toHaveBeenCalledTimes(1);
    expect(postChatMock.mock.calls[0]?.[1]).toBe("Separar o checkout em duas etapas. Revisado.");
    expect(postChatMock.mock.calls[0]?.[2]).toMatchObject({ nantyDraft: DRAFT_ID });
    expect(screen.queryByText(ptBR["conversation.nantyDraft.title"])).toBeNull();
  });

  it("não troca o texto da pessoa sem ela pedir, e o envio dela não leva o id", async () => {
    const id = "draft-busy";
    serve(null);
    postChatMock.mockResolvedValue({ ok: true, data: { reply: "Ok.", draft_updated: false, turn_id: "fedcba9876543210fedcba9876543210" } });
    render(<Harness id={id} />);
    await waitFor(() => expect(getMock).toHaveBeenCalled());
    fireEvent.change(box(), { target: { value: "meu texto" } });
    serve({ id: DRAFT_ID, text: "Texto do Nanty.", marked: false });
    act(() => bumpArchitectDraft());
    await waitFor(() => expect(screen.getByText(ptBR["conversation.nantyDraft.busy"])).toBeTruthy());
    expect(box().value).toBe("meu texto");
    expect(screen.queryByText(ptBR["conversation.nantyDraft.marked"])).toBeNull();
    expect(document.querySelector("[data-nanty-draft='clean']")).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: ptBR["conversation.nantyDraft.use"] }));
    expect(box().value).toBe("Texto do Nanty.");
    expect(screen.getByText(ptBR["conversation.nantyDraft.hint"])).toBeTruthy();
  });

  it("descartar tira o texto da caixa e avisa o servidor, no idioma da tela", async () => {
    const id = "draft-discard";
    serve({ id: DRAFT_ID, text: "Texto do Nanty.", marked: false });
    render(<Harness id={id} />);
    await waitFor(() => expect(box().value).toBe("Texto do Nanty."));
    act(() => setLanguage("en"));
    expect(screen.getByText(en["conversation.nantyDraft.hint"])).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: en["conversation.nantyDraft.discard"] }));
    expect(discardMock).toHaveBeenCalledWith(id, DRAFT_ID);
    expect((screen.getByLabelText(en["conversation.inputLabel"]) as HTMLTextAreaElement).value).toBe("");
    expect(screen.queryByText(en["conversation.nantyDraft.title"])).toBeNull();
    expect(postChatMock).not.toHaveBeenCalled();
  });

  it("reabrir a janela com o rascunho editado mantém o id", async () => {
    const id = "draft-reopen";
    serve({ id: DRAFT_ID, text: "Texto do Nanty.", marked: true });
    postChatMock.mockResolvedValue({ ok: true, data: { reply: "Ok.", draft_updated: false, turn_id: "fedcba9876543210fedcba9876543210" } });
    const first = render(<Harness id={id} />);
    await waitFor(() => expect(box().value).toBe("Texto do Nanty."));
    fireEvent.change(box(), { target: { value: "Texto editado por mim." } });
    first.unmount();
    render(<Harness id={id} />);
    await waitFor(() => expect(screen.getByText(ptBR["conversation.nantyDraft.hint"])).toBeTruthy());
    expect(box().value).toBe("Texto editado por mim.");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: ptBR["conversation.send"] }));
    });
    expect(postChatMock.mock.calls[0]?.[2]).toMatchObject({ nantyDraft: DRAFT_ID });
  });

  it("o id só sai com o sucesso: depois de uma falha, o Reenviar leva o mesmo id", async () => {
    const id = "draft-resend";
    serve({ id: DRAFT_ID, text: "Texto do Nanty.", marked: true });
    postChatMock.mockResolvedValueOnce({ ok: false, error: { kind: "http", status: 409, message: "ocupado" } });
    postChatMock.mockResolvedValueOnce({ ok: true, data: { reply: "Ok.", draft_updated: false, turn_id: "fedcba9876543210fedcba9876543210" } });
    render(<Harness id={id} />);
    await waitFor(() => expect(box().value).toBe("Texto do Nanty."));
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: ptBR["conversation.send"] }));
    });
    expect(postChatMock.mock.calls[0]?.[2]).toMatchObject({ nantyDraft: DRAFT_ID });
    expect(screen.getByText(ptBR["conversation.nantyDraft.title"])).toBeTruthy();
    expect(screen.getByText(ptBR["conversation.nantyDraft.marked"])).toBeTruthy();
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Reenviar" }));
    });
    expect(postChatMock).toHaveBeenCalledTimes(2);
    expect(postChatMock.mock.calls[1]?.[2]).toMatchObject({ nantyDraft: DRAFT_ID });
    expect(screen.queryByText(ptBR["conversation.nantyDraft.title"])).toBeNull();
  });
});
