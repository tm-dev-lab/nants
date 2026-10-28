/*
 * Cliente da queda da conexão no turno do Arquiteto (spec 127 T01): o aviso do sistema no
 * histórico (`role: system` com `code`), o 502 com `reason: connection_lost` e a continuação
 * interna (`POST /chat/continue`).
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { getChatHistory, postChat, postChatContinue } from "./client";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

const LOST = { error: "x", code: "provider_failed", params: { provider: "provider_connect_failed" }, reason: "connection_lost" };

describe("queda da conexão no turno do Arquiteto", () => {
  it("o histórico aceita o aviso do sistema com code e params saneados", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        jsonResponse(200, {
          turns: [
            { role: "user", text: "gera o plano", ts: 1 },
            { role: "system", text: "reserva", ts: 2, code: "chat.connection_lost_saved", params: { spec: "001-plano", bad: { x: 1 } } },
            { role: "system", text: "sem código", ts: 3, code: "Inválido!" },
            { role: "robot", text: "não", ts: 4 },
          ],
        }),
      ),
    );
    const result = await getChatHistory("p1");
    if (!result.ok) throw new Error("falhou");
    expect(result.data.turns).toEqual([
      { role: "user", text: "gera o plano", ts: 1 },
      { role: "system", text: "reserva", ts: 2, code: "chat.connection_lost_saved", params: { spec: "001-plano" } },
      { role: "system", text: "sem código", ts: 3 },
    ]);
  });

  it("o 502 com connection_lost leva o reason (o aviso já está no histórico)", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(502, LOST)));
    const result = await postChat("p1", "oi");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.reason).toBe("connection_lost");
    expect(result.error.providerCode).toBe("provider_connect_failed");
  });

  it("spec 133 T11: o 502 com failure_recorded leva o reason e os params da evidência", async () => {
    const params = { provider: "provider_http_404", role: "architect", model: "m", connection_id: "c", category: "bad_request", status: 404, error_type: "not_found" };
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(502, { error: "x", code: "provider_failed", params, reason: "failure_recorded" })));
    const result = await postChat("p1", "oi");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.reason).toBe("failure_recorded");
    expect(result.error.providerCode).toBe("provider_http_404");
    expect(result.error.params).toEqual(params);
  });

  it("postChatContinue chama /chat/continue, com a spec quando houver, e lê a mesma resposta", async () => {
    const spy = vi.fn(async (_url: string, _init: RequestInit) => jsonResponse(200, { reply: "ok", draft_updated: false, turn_id: "ab" }));
    vi.stubGlobal("fetch", spy);
    const plain = await postChatContinue("p1");
    expect(plain).toEqual({ ok: true, data: { reply: "ok", draft_updated: false, turn_id: "ab" } });
    const withSpec = await postChatContinue("p1", "001-plano");
    expect(withSpec.ok).toBe(true);
    const [url, init] = spy.mock.calls[0] ?? [];
    expect(url).toBe("/api/projects/p1/chat/continue");
    expect(init?.method).toBe("POST");
    expect(init?.body).toBe("{}");
    expect(spy.mock.calls[1]?.[1]?.body).toBe(JSON.stringify({ spec_id: "001-plano" }));
  });

  it("postChatContinue mapeia a falha do provedor como o postChat", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => jsonResponse(502, LOST)));
    const result = await postChatContinue("p1");
    if (result.ok) throw new Error("devia falhar");
    expect(result.error.status).toBe(502);
    expect(result.error.reason).toBe("connection_lost");
  });
});
