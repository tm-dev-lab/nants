import { afterEach, describe, expect, it, vi } from "vitest";
import {
  stopApp,
  approveSpec,
  browse,
  createProject,
  getConfig,
  resolvePermission,
  getConnectionModels,
  postChat,
  putConnection,
  request,
  resumeSpec,
  testConnection,
} from "./client";

const SECRET = "sk-SEGREDO-NAO-PODE-VAZAR-9999";

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function stubFetch(impl: (url: string, init: RequestInit) => Promise<Response>) {
  const spy = vi.fn(impl);
  vi.stubGlobal("fetch", spy);
  return spy;
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("cliente de API: erros mapeados", () => {
  it("guarda o código do servidor e a reserva do status; o texto do servidor não é repassado (spec 115 T02)", async () => {
    stubFetch(async () => jsonResponse(404, { error: "projeto não encontrado", code: "project_not_found" }));
    const result = await getConfig();
    expect(result).toEqual({
      ok: false,
      error: { kind: "http", status: 404, message: "recurso não encontrado", messageKey: "api.status.404", code: "project_not_found" },
    });
  });

  it("decisão inválida das permissões vem pelo código", async () => {
    stubFetch(async () => jsonResponse(422, { error: 'decision deve ser "once", "run", "always" ou "deny"', code: "decision_invalid" }));
    const result = await resolvePermission("p1", "r1", "once");
    expect(result).toEqual({
      ok: false,
      error: { kind: "http", status: 422, message: "dados inválidos", messageKey: "api.status.422", code: "decision_invalid" },
    });
  });

  it("não ecoa corpo cru: texto desconhecido vira mensagem do status", async () => {
    stubFetch(async () => jsonResponse(422, { error: `api_key inválida: ${SECRET}` }));
    const result = await putConnection({
      id: "c1",
      name: "C",
      base_url: "https://api.example.com",
      api_key: SECRET,
    });
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.kind).toBe("http");
    expect(result.error.status).toBe(422);
    expect(result.error.message).toBe("dados inválidos");
    expect(JSON.stringify(result)).not.toContain(SECRET);
  });

  it("status 5xx com corpo HTML ou texto livre não aparece na mensagem", async () => {
    stubFetch(async () =>
      new Response(`<html>stack trace ${SECRET}</html>`, { status: 500 }),
    );
    const result = await createProject({ name: "P", path: "/tmp/p" });
    expect(result).toEqual({
      ok: false,
      error: { kind: "http", status: 500, message: "falha interna do servidor", messageKey: "api.serverError" },
    });
    expect(JSON.stringify(result)).not.toContain(SECRET);
  });

  it("erro de rede não expõe a mensagem do TypeError", async () => {
    stubFetch(async () => {
      throw new TypeError(`failed to reach https://user:${SECRET}@host`);
    });
    const result = await getConfig();
    expect(result).toEqual({
      ok: false,
      error: {
        kind: "network",
        status: 0,
        message: "falha de comunicação com o servidor",
        messageKey: "api.network",
      },
    });
    expect(JSON.stringify(result)).not.toContain(SECRET);
  });

  it("resposta de sucesso com JSON inválido vira invalid_response", async () => {
    stubFetch(async () => new Response("não é json", { status: 200 }));
    const result = await getConfig();
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.kind).toBe("invalid_response");
  });
});

describe("cliente de API: timeout", () => {
  it("aborta a requisição e devolve kind=timeout", async () => {
    stubFetch(
      (_url, init) =>
        new Promise<Response>((_resolve, reject) => {
          init.signal?.addEventListener("abort", () => {
            reject(new DOMException("aborted", "AbortError"));
          });
        }),
    );
    const result = await request({ method: "GET", path: "/api/config", timeoutMs: 5 });
    expect(result).toEqual({
      ok: false,
      error: {
        kind: "timeout",
        status: 0,
        message: "tempo limite excedido ao falar com o servidor",
        messageKey: "api.timeout",
      },
    });
  });
});

describe("cliente de API: start/stop do app", () => {
  it("usam timeout de 60 s (parar leva > 10 s)", async () => {
    vi.useFakeTimers();
    try {
      let aborted = false;
      stubFetch(
        (_url, init) =>
          new Promise<Response>((resolve, reject) => {
            init.signal?.addEventListener("abort", () => {
              aborted = true;
              reject(new DOMException("aborted", "AbortError"));
            });
            setTimeout(() => resolve(new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } })), 12_000);
          }),
      );
      const pending = stopApp("p");
      await vi.advanceTimersByTimeAsync(12_000);
      const result = await pending;
      expect(aborted).toBe(false);
      expect(result.ok).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("cliente de API: requisições", () => {
  it("GET usa same-origin e não envia Content-Type", async () => {
    const spy = stubFetch(async () => jsonResponse(200, { server: {}, connections: [] }));
    await getConfig();
    const [url, init] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/config");
    expect(init.credentials).toBe("same-origin");
    expect(init.method).toBe("GET");
    expect((init.headers as Record<string, string>)["Content-Type"]).toBeUndefined();
  });

  it("mutações enviam Content-Type JSON", async () => {
    const spy = stubFetch(async () => jsonResponse(200, { ok: true, spec_id: "001" }));
    await approveSpec("proj-1", "001");
    const [url, init] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/projects/proj-1/specs/001/approve");
    expect(init.method).toBe("POST");
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
  });

  it("resumeSpec faz POST com corpo {} no caminho /resume e codifica os ids", async () => {
    const spy = stubFetch(async () => jsonResponse(202, { ok: true, spec_id: "001" }));
    const result = await resumeSpec("a/b", "x y");
    expect(result.ok).toBe(true);
    const [url, init] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/projects/a%2Fb/specs/x%20y/resume");
    expect(init.method).toBe("POST");
    expect(init.body).toBe("{}");
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
  });

  it("resumeSpec devolve o motivo fixo de 409 e nunca o corpo cru", async () => {
    stubFetch(async () => jsonResponse(409, { error: "não há tarefa bloqueada para retomar", code: "no_blocked_task" }));
    const result = await resumeSpec("p1", "001");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.status).toBe(409);
    expect(result.error.code).toBe("no_blocked_task");
    expect(result.error.message).toBe("operação não permitida no estado atual");
  });

  it("resumeSpec mapeia base_dirty e 422 de agente com o mesmo vocabulário do approve", async () => {
    stubFetch(async () => jsonResponse(409, { error: "a base tem alterações", reason: "base_dirty" }));
    const dirty = await resumeSpec("p1", "001");
    expect(dirty.ok).toBe(false);
    if (!dirty.ok) expect(dirty.error.reason).toBe("base_dirty");

    stubFetch(async () => jsonResponse(422, { error: "agente developer não configurado", code: "developer_not_configured" }));
    const agent = await resumeSpec("p1", "001");
    expect(agent.ok).toBe(false);
    if (!agent.ok) expect(agent.error.code).toBe("developer_not_configured");
  });

  it("resumeSpec: texto desconhecido de 409 vira mensagem do status, e falha de rede é estática", async () => {
    stubFetch(async () => jsonResponse(409, { error: SECRET }));
    const unknown = await resumeSpec("p1", "001");
    expect(unknown.ok).toBe(false);
    if (!unknown.ok) {
      expect(unknown.error.message).toBe("operação não permitida no estado atual");
      expect(JSON.stringify(unknown)).not.toContain(SECRET);
    }

    stubFetch(async () => {
      throw new TypeError("connection reset");
    });
    const network = await resumeSpec("p1", "001");
    expect(network.ok).toBe(false);
    if (!network.ok) expect(network.error.kind).toBe("network");
  });

  it("api_key só aparece no corpo de putConnection, nunca na URL ou cabeçalhos", async () => {
    const spy = stubFetch(async () => jsonResponse(200, { id: "c1" }));
    await putConnection({
      id: "c1",
      name: "C",
      base_url: "https://api.example.com",
      api_key: SECRET,
    });
    const [url, init] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).not.toContain(SECRET);
    expect(JSON.stringify(init.headers)).not.toContain(SECRET);
    expect(String(init.body)).toContain(SECRET);
  });

  it("testConnection envia api_key só no corpo e usa a rota de teste", async () => {
    const spy = stubFetch(async () => jsonResponse(200, { ok: true, latency_ms: 10 }));
    const result = await testConnection({ id: "c1", api_key: SECRET });
    expect(result).toEqual({ ok: true, data: { ok: true, latency_ms: 10 } });
    const [url] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/config/connections/test");
  });

  it("testConnection sem modelo não envia o campo model (teste pela listagem)", async () => {
    const spy = stubFetch(async () => jsonResponse(200, { ok: true, latency_ms: 3, models_count: 2 }));
    const result = await testConnection({ id: "c1" });
    expect(result).toEqual({ ok: true, data: { ok: true, latency_ms: 3, models_count: 2 } });
    const [, init] = spy.mock.calls[0] as [string, RequestInit];
    expect(String(init.body)).toBe(JSON.stringify({ id: "c1" }));
  });

  it("browse monta a query sem campos vazios", async () => {
    const spy = stubFetch(async () =>
      jsonResponse(200, { path: "/home", parent: "/", entries: [], truncated: false }),
    );
    await browse({ path: "/home/u s", hidden: true });
    const [url] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/fs/browse?path=%2Fhome%2Fu+s&hidden=true");
  });

  it("codifica ids de projeto no caminho", async () => {
    const spy = stubFetch(async () => jsonResponse(200, {}));
    await approveSpec("a/b", "x y");
    const [url] = spy.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/projects/a%2Fb/specs/x%20y/approve");
  });
});

describe("504 timeout do chat", () => {
  it("504 com code chat_timeout vira kind timeout (não erro de servidor)", async () => {
    stubFetch(async () => jsonResponse(504, { error: "o turno do Arquiteto excedeu o tempo limite", code: "chat_timeout" }));
    const result = await postChat("p1", "oi");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.kind).toBe("timeout");
    expect(result.error.message).toBe("tempo limite excedido ao falar com o servidor");
  });
});

describe("502 do chat: causa fixa do provedor", () => {
  it.each([
    ["provider_http_400", "O provedor recusou a requisição (HTTP 400). Verifique o modelo e o protocolo da conexão."],
    ["provider_http_401", "O provedor recusou a chave de API (HTTP 401). Verifique a conexão."],
    ["provider_http_429", "O provedor limitou as requisições (HTTP 429). O Nants já reduz o ritmo sozinho; continue em alguns minutos."],
    ["provider_timeout", "O provedor não respondeu a tempo."],
    ["provider_connect_failed", "Não foi possível conectar ao provedor. Verifique a URL da conexão."],
    ["provider_bad_response", "O provedor devolveu uma resposta inesperada."],
    ["provider_invalid_config", "A conexão está mal configurada (chave ou URL inválida)."],
  ])("%s vira mensagem fixa e guarda o código", async (code, text) => {
    stubFetch(async () => jsonResponse(502, { error: "o provedor do modelo falhou", code: "provider_failed", params: { provider: code } }));
    const result = await postChat("p1", "oi");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.status).toBe(502);
    expect(result.error.message).toBe(text);
    expect(result.error.providerCode).toBe(code);
  });

  it("código de provedor fora do vocabulário é 'falha interna' e nunca aparece cru", async () => {
    stubFetch(async () => jsonResponse(502, { error: `provider_http_600 ${SECRET}`, code: "provider_failed", params: { provider: "provider_http_600" } }));
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.message).toBe("O provedor devolveu uma resposta inesperada.");
    expect(result.error.providerCode).toBeUndefined();
    expect(JSON.stringify(result)).not.toContain(SECRET);
  });

  it("502 com texto desconhecido (ex.: backend_error antigo) não é exibido cru", async () => {
    stubFetch(async () => jsonResponse(502, { error: "backend_error" }));
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.message).toBe("O provedor devolveu uma resposta inesperada.");
    expect(result.error.providerCode).toBeUndefined();
  });

  it.each([
    ["provider_bad_response", { error: "provider_bad_response" }],
    ["bad_response legado (servidor antigo)", { error: "bad_response" }],
    ["backend_error legado", { error: "backend_error" }],
    ["texto desconhecido", { error: `vazou ${SECRET}` }],
    ["corpo sem error", {}],
  ])("502 com %s mostra a resposta inesperada, nunca falha interna", async (_label, body) => {
    stubFetch(async () => jsonResponse(502, body));
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.status).toBe(502);
    expect(result.error.message).toBe("O provedor devolveu uma resposta inesperada.");
    expect(JSON.stringify(result)).not.toContain(SECRET);
  });

  it("detail do 502 entra entre parênteses e fica guardado (spec 088)", async () => {
    stubFetch(async () =>
      jsonResponse(502, { error: "x", detail: "sem texto nem ação", code: "provider_failed", params: { provider: "provider_bad_response" } }),
    );
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.message).toBe("O provedor devolveu uma resposta inesperada (sem texto nem ação).");
    expect(result.error.providerCode).toBe("provider_bad_response");
    expect(result.error.providerDetail).toBe("sem texto nem ação");
  });

  it.each([
    ["longo demais", { error: "provider_bad_response", detail: `${SECRET}${"x".repeat(81)}` }],
    ["com quebra de linha", { error: "provider_bad_response", detail: `vazia\n${SECRET}` }],
    ["não texto", { error: "provider_bad_response", detail: { segredo: SECRET } }],
    ["com código desconhecido", { error: "backend_error", detail: SECRET }],
  ])("detail inválido (%s) é ignorado", async (_label, body) => {
    stubFetch(async () => jsonResponse(502, body));
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.message).toBe("O provedor devolveu uma resposta inesperada.");
    expect(result.error.providerDetail).toBeUndefined();
    expect(JSON.stringify(result)).not.toContain(SECRET);
  });

  it("504 continua com tempo limite, mesmo com código de provedor no corpo", async () => {
    stubFetch(async () => jsonResponse(504, { error: "provider_bad_response" }));
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.kind).toBe("http");
    expect(result.error.message).toBe("falha interna do servidor");
  });

  it("500 continua 'falha interna do servidor' mesmo com corpo de provedor", async () => {
    stubFetch(async () => jsonResponse(500, { error: "provider_http_400" }));
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.message).toBe("falha interna do servidor");
    expect(result.error.providerCode).toBeUndefined();
  });
});

describe("cliente de API: listagem de modelos", () => {
  it("GET da conexão salva sem refresh e com refresh=true; id é codificado", async () => {
    const spy = stubFetch(async () =>
      jsonResponse(200, { connection_id: "c", models: [{ id: "gpt-4o" }], truncated: false, fetched_at: 1760000000 }),
    );
    const plain = await getConnectionModels("c");
    expect(plain.ok).toBe(true);
    expect(spy.mock.calls[0]?.[0]).toBe("/api/config/connections/c/models");
    expect(spy.mock.calls[0]?.[1]?.method).toBe("GET");

    await getConnectionModels("a b/c", { refresh: true });
    expect(spy.mock.calls[1]?.[0]).toBe("/api/config/connections/a%20b%2Fc/models?refresh=true");
  });

  it("falha do provedor (200 com error) chega ao chamador como dado, não como erro de transporte", async () => {
    stubFetch(async () => jsonResponse(200, { models: [], error: "http_error", status: 401 }));
    const result = await getConnectionModels("c");
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.data.error).toBe("http_error");
  });

  it("corpo sem lista de modelos vira resposta inválida", async () => {
    stubFetch(async () => jsonResponse(200, { connection_id: "c" }));
    const result = await getConnectionModels("c");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.kind).toBe("invalid_response");
  });

  it("404 de conexão inexistente vira erro de HTTP sem o corpo cru", async () => {
    stubFetch(async () => jsonResponse(404, { error: SECRET }));
    const result = await getConnectionModels("nope");
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.status).toBe(404);
      expect(JSON.stringify(result)).not.toContain(SECRET);
    }
  });
});

describe("contrato: corpo JSON em POST/PUT/DELETE", () => {
  it("toda chamada nao-GET envia Content-Type JSON, mesmo sem corpo", async () => {
    const c = await import("./client");
    const spy = stubFetch(async () => jsonResponse(200, {}));
    await c.autofillPricing(false);
    await c.autofillPricing(true);
    await c.removeProject("p");
    await c.approveSpec("p", "s");
    await c.resumeSpec("p", "s");
    await c.startApp("p", "s");
    await c.stopApp("p");
    await request({ method: "POST", path: "/x" });
    await request({ method: "PUT", path: "/x" });
    expect(spy.mock.calls.length).toBe(9);
    for (const [, init] of spy.mock.calls) {
      const h = (init as RequestInit).headers as Record<string, string>;
      expect(h["Content-Type"]).toBe("application/json");
    }
    expect((spy.mock.calls[0]?.[1] as RequestInit).body).toBe("{}");
  });
});

describe("contrato: GET nunca leva corpo", () => {
  it("fetch que lanca TypeError para GET com corpo nao quebra as funcoes GET", async () => {
    const c = await import("./client");
    const spy = stubFetch(async (_u, init) => {
      if ((init.method === "GET" || init.method === "HEAD") && (init.body !== undefined && init.body !== null)) {
        throw new TypeError("Request with GET/HEAD method cannot have body");
      }
      if (init.method === "GET") {
        const h = init.headers as Record<string, string>;
        expect(h["Content-Type"]).toBeUndefined();
      }
      return jsonResponse(200, {});
    });
    const results = [
      await c.getPriceEstimate(["m"]),
      await c.getConfig(),
      await c.getConnectionModels("c"),
      await request({ method: "GET", path: "/x", body: "{}" }),
    ];
    for (const r of results) expect(r.ok || r.error.kind !== "network").toBe(true);
    expect(spy).toHaveBeenCalledTimes(4);
    const post = await request({ method: "POST", path: "/x" });
    expect(post.ok).toBe(true);
    expect((spy.mock.calls[4]?.[1] as RequestInit).body).toBe("{}");
  });
});
