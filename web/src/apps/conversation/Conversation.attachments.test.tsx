/* Imagens na janela Conversa (spec 093, T06): anexar, colar, arrastar e soltar. */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { appStore, useAppStore } from "../../store";
import type { ChatMessage } from "../../store";
import { postChat, uploadChatAttachment } from "../../api/client";
import type { ApiResult } from "../../api/client";
import type { ChatAttachmentUpload } from "../../api/types";
import { prepareImage } from "../../imageResize";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import {
  ATTACH_LABEL,
  ATTACHMENTS_FAILED_NOTICE,
  ATTACHMENTS_LIMIT_NOTICE,
  ATTACHED_IMAGE_TITLE,
  ATTACHMENTS_PREVIEW_LABEL,
  THUMB_PLACEHOLDER_TEXT,
  USER_IMAGES_LABEL,
} from "./ChatAttachments";

vi.mock("../../api/client", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../../api/client")>()),
  postChat: vi.fn(),
  uploadChatAttachment: vi.fn(),
}));
vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));
vi.mock("../../imageResize", () => ({ prepareImage: vi.fn() }));

const postChatMock = vi.mocked(postChat);
const uploadMock = vi.mocked(uploadChatAttachment);
const prepareMock = vi.mocked(prepareImage);
const NONE: ChatMessage[] = [];

function name(n: number): string {
  return `${n.toString(16).padStart(32, "0")}.jpg`;
}

let uploads = 0;
const DATA_URL = "data:image/png;base64,AQID";
/** Falha a próxima leitura do `FileReader` simulado. */
let readFails = false;

/** `FileReader` simulado: devolve `DATA_URL` (ou erro) num microtask. */
class FakeReader {
  result: string | null = null;
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  readAsDataURL(): void {
    const fail = readFails;
    void Promise.resolve().then(() => {
      if (fail) this.onerror?.();
      else {
        this.result = DATA_URL;
        this.onload?.();
      }
    });
  }
}

const createObjectURL = vi.fn(() => "blob:previa");

beforeEach(() => {
  uploads = 0;
  readFails = false;
  createObjectURL.mockClear();
  // A prévia não usa URL de objeto (spec 084 T03): o espião prova que não é chamado.
  Object.assign(URL, { createObjectURL, revokeObjectURL: vi.fn() });
  vi.stubGlobal("FileReader", FakeReader);
  prepareMock.mockImplementation(async (file) => ({ blob: file, type: "image/jpeg" }));
  uploadMock.mockImplementation(async () => {
    uploads += 1;
    return { ok: true, data: { id: name(uploads).slice(0, 32), name: name(uploads) } };
  });
  postChatMock.mockResolvedValue({ ok: true, data: { reply: "Vi a imagem.", draft_updated: false, turn_id: "abc" } });
});

afterEach(() => {
  expect(createObjectURL).not.toHaveBeenCalled();
  cleanup();
  vi.unstubAllGlobals();
  postChatMock.mockReset();
  uploadMock.mockReset();
  prepareMock.mockReset();
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

function image(label = "foto.png"): File {
  return new File([new Uint8Array([1, 2, 3])], label, { type: "image/png" });
}

function pick(files: File[]) {
  const input = screen.getByTestId("chat-attach-input") as HTMLInputElement;
  expect(input.accept).toBe("image/*");
  fireEvent.change(input, { target: { files } });
}

function preview() {
  return screen.queryByRole("list", { name: ATTACHMENTS_PREVIEW_LABEL });
}

describe("imagens na caixa do Arquiteto (spec 082 T05)", () => {
  it("o botão de clipe abre o seletor e a imagem anexada aparece em prévia e sobe", async () => {
    render(<Harness id="att-pick" />);
    const input = screen.getByTestId("chat-attach-input") as HTMLInputElement;
    const click = vi.spyOn(input, "click");
    fireEvent.click(screen.getByRole("button", { name: ATTACH_LABEL }));
    expect(click).toHaveBeenCalled();
    pick([image()]);
    expect(within(preview() as HTMLElement).getAllByRole("listitem")).toHaveLength(1);
    await waitFor(() => expect(uploadMock).toHaveBeenCalledTimes(1));
    expect(uploadMock.mock.calls[0]?.[0]).toBe("att-pick");
    expect(uploadMock.mock.calls[0]?.[2]).toBe("image/jpeg");
    await waitFor(() => expect(screen.queryByText("enviando")).toBeNull());
  });

  it("mostra 'enviando' e o erro do upload com mensagem clara", async () => {
    let finish: (r: ApiResult<ChatAttachmentUpload>) => void = () => {};
    uploadMock.mockReturnValue(new Promise((r) => (finish = r)));
    render(<Harness id="att-error" />);
    pick([image()]);
    expect(await screen.findByText("enviando")).not.toBeNull();
    await act(async () => finish({ ok: false, error: { kind: "http", status: 413, message: "imagem maior que 5 MiB" } }));
    expect(screen.getByRole("alert").textContent).toContain("imagem maior que 5 MiB");
    // Enviar com imagem em erro não manda a mensagem.
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    expect(await screen.findByText(ATTACHMENTS_FAILED_NOTICE)).not.toBeNull();
    expect(postChatMock).not.toHaveBeenCalled();
  });

  it("remove uma imagem da prévia", async () => {
    render(<Harness id="att-remove" />);
    pick([image("a.png"), image("b.png")]);
    expect(within(preview() as HTMLElement).getAllByRole("listitem")).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "Remover imagem 1" }));
    expect(within(preview() as HTMLElement).getAllByRole("listitem")).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Remover imagem 1" }));
    expect(preview()).toBeNull();
  });

  it("aceita até 4 imagens; a 5ª mostra o aviso", async () => {
    render(<Harness id="att-limit" />);
    pick([image("1.png"), image("2.png"), image("3.png")]);
    expect(screen.queryByText(ATTACHMENTS_LIMIT_NOTICE)).toBeNull();
    pick([image("4.png"), image("5.png")]);
    expect(within(preview() as HTMLElement).getAllByRole("listitem")).toHaveLength(4);
    expect(screen.getByText(ATTACHMENTS_LIMIT_NOTICE)).not.toBeNull();
    await waitFor(() => expect(uploadMock).toHaveBeenCalledTimes(4));
  });

  it("colar uma imagem (Ctrl+V) anexa; texto colado não", () => {
    render(<Harness id="att-paste" />);
    const area = screen.getByLabelText("Mensagem para o Arquiteto");
    fireEvent.paste(area, { clipboardData: { items: [{ kind: "string", type: "text/plain", getAsFile: () => null }] } });
    expect(preview()).toBeNull();
    const file = image("colada.png");
    fireEvent.paste(area, { clipboardData: { items: [{ kind: "file", type: "image/png", getAsFile: () => file }] } });
    expect(within(preview() as HTMLElement).getAllByRole("listitem")).toHaveLength(1);
  });

  it("Enviar aguarda o upload e manda attachments; só imagem vira '(imagem)'", async () => {
    let finish: (r: ApiResult<ChatAttachmentUpload>) => void = () => {};
    uploadMock.mockReturnValue(new Promise((r) => (finish = r)));
    render(<Harness id="att-send" />);
    pick([image()]);
    const send = screen.getByRole("button", { name: "Enviar" }) as HTMLButtonElement;
    expect(send.disabled).toBe(false); // sem texto, mas com imagem
    fireEvent.click(send);
    await Promise.resolve();
    expect(postChatMock).not.toHaveBeenCalled();
    await act(async () => finish({ ok: true, data: { id: name(9).slice(0, 32), name: name(9) } }));
    await waitFor(() => expect(postChatMock).toHaveBeenCalledTimes(1));
    expect(postChatMock).toHaveBeenCalledWith("att-send", "(imagem)", expect.objectContaining({ attachments: [name(9)] }));
    expect(preview()).toBeNull();
    // O balão do usuário mostra a miniatura.
    const thumbs = await screen.findByRole("list", { name: USER_IMAGES_LABEL });
    const img = within(thumbs).getByRole("img") as HTMLImageElement;
    expect(img.getAttribute("src")).toBe(`/api/projects/att-send/chat/attachments/${name(9)}`);
  });

  it("texto e imagem vão juntos; sem imagem nada de attachments", async () => {
    render(<Harness id="att-text" />);
    const area = screen.getByLabelText("Mensagem para o Arquiteto");
    fireEvent.change(area, { target: { value: "só texto" } });
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    await waitFor(() => expect(postChatMock).toHaveBeenCalledTimes(1));
    expect(postChatMock.mock.calls[0]?.[2]).not.toHaveProperty("attachments");
    pick([image()]);
    await waitFor(() => expect(uploadMock).toHaveBeenCalledTimes(1));
    fireEvent.change(area, { target: { value: "veja a tela" } });
    fireEvent.click(screen.getByRole("button", { name: "Enviar" }));
    await waitFor(() => expect(postChatMock).toHaveBeenCalledTimes(2));
    expect(postChatMock.mock.calls[1]?.[1]).toBe("veja a tela");
    expect(postChatMock.mock.calls[1]?.[2]).toEqual(expect.objectContaining({ attachments: [name(1)] }));
  });

  it("miniatura do histórico abre o visualizador ampliado", async () => {
    const id = "att-history";
    act(() => {
      appStore.hydrateHistory(id, [
        { role: "user", text: "olha", ts: 1, turn_id: "t1", attachments: [name(5)] },
        { role: "architect", text: "vi", ts: 2, turn_id: "t1" },
      ]);
    });
    render(<Harness id={id} />);
    const thumbs = screen.getByRole("list", { name: USER_IMAGES_LABEL });
    const img = within(thumbs).getByRole("img");
    expect(img.getAttribute("src")).toBe(`/api/projects/${id}/chat/attachments/${name(5)}`);
    fireEvent.click(within(thumbs).getByRole("button", { name: "Ampliar imagem enviada" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("img").getAttribute("src")).toBe(`/api/projects/${id}/chat/attachments/${name(5)}`);
    fireEvent.click(within(dialog).getByRole("button", { name: "Fechar" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
});

describe("miniatura do anexo (spec 084 T03)", () => {
  function thumbImg(): HTMLImageElement | null {
    return within(preview() as HTMLElement).queryByRole("img") as HTMLImageElement | null;
  }

  it("prévia em data URL enquanto sobe; URL do servidor ao ficar ready", async () => {
    let finish: (r: ApiResult<ChatAttachmentUpload>) => void = () => {};
    uploadMock.mockReturnValue(new Promise((r) => (finish = r)));
    render(<Harness id="att-src" />);
    pick([image()]);
    await waitFor(() => expect(thumbImg()?.getAttribute("src")).toBe(DATA_URL));
    // Caixa própria da miniatura (estilo pelos tokens).
    const box = screen.getByRole("button", { name: "Ampliar imagem 1" });
    expect(box.className).toContain("nd-draft-thumb");
    await act(async () => finish({ ok: true, data: { id: name(7).slice(0, 32), name: name(7) } }));
    await waitFor(() => expect(thumbImg()?.getAttribute("src")).toBe(`/api/projects/att-src/chat/attachments/${name(7)}`));
    expect(createObjectURL).not.toHaveBeenCalled();
  });

  it("sem FileReader ou com falha na leitura mostra o marcador", async () => {
    uploadMock.mockReturnValue(new Promise(() => {}));
    readFails = true;
    render(<Harness id="att-noreader" />);
    pick([image()]);
    await act(async () => {}); // a leitura falha
    expect(thumbImg()).toBeNull();
    expect(within(preview() as HTMLElement).getByText(THUMB_PLACEHOLDER_TEXT)).not.toBeNull();
    cleanup();
    vi.stubGlobal("FileReader", undefined);
    render(<Harness id="att-noreader-2" />);
    pick([image()]);
    expect(thumbImg()).toBeNull();
    expect(within(preview() as HTMLElement).getByText(THUMB_PLACEHOLDER_TEXT)).not.toBeNull();
  });

  it("erro de carregamento mostra o marcador; ao ficar ready tenta a URL do servidor uma vez", async () => {
    let finish: (r: ApiResult<ChatAttachmentUpload>) => void = () => {};
    uploadMock.mockReturnValue(new Promise((r) => (finish = r)));
    render(<Harness id="att-broken" />);
    pick([image()]);
    await waitFor(() => expect(thumbImg()).not.toBeNull());
    fireEvent.error(thumbImg() as HTMLImageElement);
    expect(thumbImg()).toBeNull();
    expect(within(preview() as HTMLElement).getByText(THUMB_PLACEHOLDER_TEXT)).not.toBeNull();
    await act(async () => finish({ ok: true, data: { id: name(3).slice(0, 32), name: name(3) } }));
    await waitFor(() => expect(thumbImg()?.getAttribute("src")).toBe(`/api/projects/att-broken/chat/attachments/${name(3)}`));
    // A URL do servidor também falha: fica o marcador, sem nova tentativa.
    fireEvent.error(thumbImg() as HTMLImageElement);
    expect(thumbImg()).toBeNull();
    expect(within(preview() as HTMLElement).getByText(THUMB_PLACEHOLDER_TEXT)).not.toBeNull();
  });

  it("tocar na miniatura abre o visualizador; remover não", async () => {
    render(<Harness id="att-zoom" />);
    pick([image("a.png"), image("b.png")]);
    await waitFor(() => expect(within(preview() as HTMLElement).getAllByRole("img")).toHaveLength(2));
    fireEvent.click(screen.getByRole("button", { name: "Remover imagem 2" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    const thumb = screen.getByRole("button", { name: "Ampliar imagem 1" });
    thumb.focus();
    fireEvent.click(thumb);
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText(ATTACHED_IMAGE_TITLE)).not.toBeNull();
    const src = within(dialog).getByRole("img").getAttribute("src");
    expect(src === DATA_URL || src === `/api/projects/att-zoom/chat/attachments/${name(1)}`).toBe(true);
    fireEvent.click(within(dialog).getByRole("button", { name: "Fechar" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(thumb));
  });
});

describe("arrastar e soltar imagens na Conversa (spec 093, T06)", () => {
  function files(list: File[]) {
    return { dataTransfer: { types: ["Files"], files: list, dropEffect: "none" } };
  }

  it("soltar anexa só as imagens e os eventos não chegam ao desktop", async () => {
    const outside = vi.fn();
    render(
      <div onDrop={outside} onDragOver={outside} onDragEnter={outside}>
        <Harness id="att-drop" />
      </div>,
    );
    const zone = document.querySelector("[data-conversation]") as HTMLElement;
    fireEvent.dragEnter(zone, files([image()]));
    expect(zone.getAttribute("data-dragging")).toBe("true");
    const over = fireEvent.dragOver(zone, files([image()]));
    expect(over).toBe(false); // preventDefault: o navegador não abre o arquivo
    const text = new File(["x"], "nota.txt", { type: "text/plain" });
    fireEvent.drop(zone, files([image("solta.png"), text]));
    expect(zone.getAttribute("data-dragging")).toBeNull();
    expect(within(preview() as HTMLElement).getAllByRole("listitem")).toHaveLength(1);
    expect(outside).not.toHaveBeenCalled();
    await waitFor(() => expect(uploadMock).toHaveBeenCalledTimes(1));
  });

  it("arrastar texto (sem arquivos) não é tratado pela Conversa", () => {
    render(<Harness id="att-drag-text" />);
    const zone = document.querySelector("[data-conversation]") as HTMLElement;
    fireEvent.dragEnter(zone, { dataTransfer: { types: ["text/plain"], files: [] } });
    expect(zone.getAttribute("data-dragging")).toBeNull();
  });

  it("nenhum controle do campo usa caractere no lugar de ícone", () => {
    const { container } = render(<Harness id="att-glyphs" />);
    pick([image()]);
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });
});
