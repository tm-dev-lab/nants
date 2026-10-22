import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ATTACHMENT_NO_SPACE,
  ATTACHMENT_TOO_LARGE,
  ATTACHMENT_UNSUPPORTED,
  getChatHistory,
  postChat,
  uploadChatAttachment,
} from "./client";
import { chatAttachmentUrl } from "../chatImages";

const NAME = "0123456789abcdef0123456789abcdef.jpg";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

function stubFetch(impl: (url: string, init: RequestInit) => Promise<Response>) {
  const spy = vi.fn(impl);
  vi.stubGlobal("fetch", spy);
  return spy;
}

afterEach(() => vi.unstubAllGlobals());

describe("upload de imagem do chat (spec 082 T05)", () => {
  it("manda os bytes com o Content-Type da imagem e valida a resposta", async () => {
    const spy = stubFetch(async () => jsonResponse(201, { id: NAME.slice(0, 32), name: NAME }));
    const blob = new Blob([new Uint8Array([0xff, 0xd8, 0xff])], { type: "image/jpeg" });
    const result = await uploadChatAttachment("p 1", blob, "image/jpeg");
    expect(result).toEqual({ ok: true, data: { id: NAME.slice(0, 32), name: NAME } });
    const [url, init] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/projects/p%201/chat/attachments");
    expect(init.method).toBe("POST");
    expect(init.body).toBe(blob);
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("image/jpeg");
  });

  it.each([
    [413, ATTACHMENT_TOO_LARGE, "api.attachmentTooLarge"],
    [415, ATTACHMENT_UNSUPPORTED, "api.attachmentUnsupported"],
    [507, ATTACHMENT_NO_SPACE, "api.attachmentNoSpace"],
  ])("HTTP %s vira mensagem clara (com a chave do idioma ativo, spec 102 T06)", async (status, message, messageKey) => {
    stubFetch(async () => jsonResponse(status, { error: "qualquer" }));
    const result = await uploadChatAttachment("p1", new Blob(["x"]), "image/png");
    expect(result).toEqual({ ok: false, error: { kind: "http", status, message, messageKey } });
  });

  it("nome fora do formato vira resposta inválida", async () => {
    stubFetch(async () => jsonResponse(201, { id: "x", name: "../x.png" }));
    const result = await uploadChatAttachment("p1", new Blob(["x"]), "image/png");
    expect(result.ok).toBe(false);
  });

  it("postChat leva attachments só quando há imagens", async () => {
    const spy = stubFetch(async () => jsonResponse(200, { reply: "ok", draft_updated: false, turn_id: "ab" }));
    await postChat("p1", "oi", { attachments: [NAME] });
    await postChat("p1", "oi", { attachments: [] });
    const bodies = spy.mock.calls.map(([, init]) => JSON.parse(String((init as RequestInit).body)) as unknown);
    expect(bodies[0]).toEqual({ message: "oi", attachments: [NAME] });
    expect(bodies[1]).toEqual({ message: "oi" });
  });

  it("histórico traz só nomes válidos de imagens", async () => {
    stubFetch(async () =>
      jsonResponse(200, {
        turns: [
          { role: "user", text: "a", ts: 1, attachments: [NAME, "../x.png", 3] },
          { role: "user", text: "b", ts: 2, attachments: [] },
          { role: "architect", text: "c", ts: 3 },
        ],
      }),
    );
    const result = await getChatHistory("p1");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.data.turns[0]?.attachments).toEqual([NAME]);
    expect(result.data.turns[1]?.attachments).toBeUndefined();
    expect(result.data.turns[2]?.attachments).toBeUndefined();
  });

  it("URL da imagem codifica os segmentos", () => {
    expect(chatAttachmentUrl("a/b", NAME)).toBe(`/api/projects/a%2Fb/chat/attachments/${NAME}`);
  });
});
