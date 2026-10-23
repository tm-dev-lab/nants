import { afterEach, describe, expect, it, vi } from "vitest";
import { removeProject } from "./client";
import { KNOWN_EVENT_TYPES, parseApiEvent } from "./events";
import { installFetch } from "../test-utils";
import { apiFailureText } from "../i18n/serverCodes";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("removeProject (cliente)", () => {
  it("envia DELETE no path do projeto com corpo {} e Content-Type JSON", async () => {
    const mock = installFetch(() => ({ status: 200, body: { ok: true, removed: "p1" } }));

    const result = await removeProject("p1");

    expect(result).toEqual({ ok: true, data: { removed: "p1", alreadyRemoved: false } });
    expect(mock).toHaveBeenCalledTimes(1);
    const [url, init] = mock.mock.calls[0] ?? [];
    expect(url).toBe("/api/projects/p1");
    expect(init?.method).toBe("DELETE");
    expect(init?.body).toBe("{}");
    expect((init?.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
  });

  it("codifica o id no path", async () => {
    const mock = installFetch(() => ({ status: 200, body: { ok: true, removed: "a/b" } }));

    await removeProject("a/b");

    expect(mock.mock.calls[0]?.[0]).toBe("/api/projects/a%2Fb");
  });

  it("404 com 'projeto não encontrado' é sucesso idempotente (alreadyRemoved)", async () => {
    installFetch(() => ({ status: 404, body: { error: "projeto não encontrado", code: "project_not_found" } }));

    const result = await removeProject("p1");

    expect(result).toEqual({ ok: true, data: { removed: "p1", alreadyRemoved: true } });
  });

  it("409 de execução devolve a mensagem fixa e não é sucesso", async () => {
    installFetch(() => ({ status: 409, body: { error: "já existe uma execução em andamento neste projeto", code: "run_active" } }));

    const result = await removeProject("p1");

    expect(result).toEqual({
      ok: false,
      error: { kind: "http", status: 409, message: "operação não permitida no estado atual", messageKey: "api.status.409", code: "run_active" },
    });
  });

  it("409 de turno de chat devolve a mensagem fixa", async () => {
    installFetch(() => ({ status: 409, body: { error: "já existe um turno de chat em andamento para este projeto", code: "chat_active" } }));

    const result = await removeProject("p1");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.code).toBe("chat_active");
  });

  it("422 com texto desconhecido vira mensagem do status, sem eco do corpo", async () => {
    installFetch(() => ({ status: 422, body: { error: "id <b>x</b> inválido" } }));

    const result = await removeProject("p1");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.message).toBe("dados inválidos");
  });

  it("500 com 'falha ao salvar a configuração' repassa a mensagem fixa", async () => {
    installFetch(() => ({ status: 500, body: { error: "falha ao salvar a configuração", code: "config_save_failed" } }));

    const result = await removeProject("p1");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(apiFailureText(result.error, "pt-BR")).toBe("falha ao salvar a configuração");
  });

  it("500 sem texto conhecido vira mensagem genérica de servidor", async () => {
    installFetch(() => ({ status: 500, body: { error: "stack trace cru" } }));

    const result = await removeProject("p1");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.message).toBe("falha interna do servidor");
  });

  it("falha de rede vira mensagem de comunicação", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("offline"))));

    const result = await removeProject("p1");

    expect(result).toEqual({
      ok: false,
      error: { kind: "network", status: 0, message: "falha de comunicação com o servidor", messageKey: "api.network" },
    });
  });

  it("200 sem `removed` textual é resposta inesperada", async () => {
    installFetch(() => ({ status: 200, body: { ok: true } }));

    const result = await removeProject("p1");

    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.kind).toBe("invalid_response");
  });
});

describe("evento project_removed", () => {
  it("é um tipo conhecido do stream", () => {
    expect(KNOWN_EVENT_TYPES).toContain("project_removed");
  });

  it("aceita payload {project_id: string}", () => {
    const raw = { event_type: "project_removed", project_id: "p1", payload: { project_id: "p1" } };
    expect(parseApiEvent(raw)).toEqual(raw);
  });

  it("rejeita payload sem project_id textual", () => {
    expect(parseApiEvent({ event_type: "project_removed", project_id: "p1", payload: {} })).toBeNull();
    expect(parseApiEvent({ event_type: "project_removed", project_id: "p1", payload: { project_id: 3 } })).toBeNull();
  });
});
