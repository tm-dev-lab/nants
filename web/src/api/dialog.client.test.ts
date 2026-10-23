import { afterEach, describe, expect, it, vi } from "vitest";
import { getDialog } from "./client";
import { parseApiEvent } from "./events";
import { installFetch } from "../test-utils";

const SEQ = 1_700_000_000_000_001;

/** Linha no formato exato de `GET /api/projects/:id/specs/:spec_id/dialog`. */
const ROW = {
  seq: SEQ,
  ts: 1_700_000_000,
  agent: "developer",
  task_id: "T01",
  attempt: 1,
  text: "linha 1\nlinha 2",
};

function page(messages: unknown[], overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { messages, last_seq: SEQ, truncated_before: false, ...overrides };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("getDialog (cliente)", () => {
  it("monta after e limit na URL e devolve as mensagens validadas", async () => {
    const mock = installFetch(() => ({ body: page([ROW]) }));

    const result = await getDialog("p1", "s1", { after: 5, limit: 200 });

    expect(mock.mock.calls[0]?.[0]).toBe("/api/projects/p1/specs/s1/dialog?after=5&limit=200");
    expect(result).toEqual({
      ok: true,
      data: { messages: [ROW], last_seq: SEQ, truncated_before: false },
    });
  });

  it("sem opções não envia query", async () => {
    const mock = installFetch(() => ({ body: page([]) }));

    await getDialog("p1", "s1");

    expect(mock.mock.calls[0]?.[0]).toBe("/api/projects/p1/specs/s1/dialog");
  });

  it("aceita task_id e attempt nulos ou ausentes (viram null)", async () => {
    installFetch(() => ({
      body: page([
        { seq: 1, ts: 1, agent: "system", task_id: null, attempt: null, text: "a" },
        { seq: 2, ts: 2, agent: "reviewer", text: "b" },
      ]),
    }));

    const result = await getDialog("p1", "s1");

    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.data.messages.map((m) => [m.task_id, m.attempt])).toEqual([
      [null, null],
      [null, null],
    ]);
  });

  it.each([
    ["seq como texto", { seq: "1" }],
    ["seq fracionário", { seq: 1.5 }],
    ["seq acima do inteiro seguro", { seq: 2 ** 60 }],
    ["ts como texto", { ts: "1700" }],
    ["agent numérico", { agent: 3 }],
    ["text nulo", { text: null }],
    ["task_id numérico", { task_id: 7 }],
    ["attempt texto", { attempt: "1" }],
  ])("tipo errado (%s) vira falha de resposta inválida", async (_label, patch) => {
    installFetch(() => ({ body: page([{ ...ROW, ...patch }]) }));

    const result = await getDialog("p1", "s1");

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.kind).toBe("invalid_response");
  });

  it.each([
    ["messages não é lista", { messages: "x" }],
    ["last_seq ausente", { last_seq: undefined }],
    ["truncated_before não booleano", { truncated_before: "não" }],
  ])("envelope inválido (%s) vira falha", async (_label, patch) => {
    installFetch(() => ({ body: { ...page([]), ...patch } }));

    const result = await getDialog("p1", "s1");

    expect(result.ok).toBe(false);
  });

  it("404 vira falha estática com o código do servidor (o texto dele não é repassado)", async () => {
    installFetch(() => ({ status: 404, body: { error: "spec não encontrada", code: "spec_not_found" } }));

    const result = await getDialog("p1", "s1");

    expect(result).toEqual({
      ok: false,
      error: { kind: "http", status: 404, message: "recurso não encontrado", messageKey: "api.status.404", code: "spec_not_found" },
    });
  });

  it("rede indisponível vira falha de comunicação", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("offline"))));

    const result = await getDialog("p1", "s1");

    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.kind).toBe("network");
  });
});

describe("evento agent_message com task_id, attempt e seq", () => {
  const envelope = (payload: Record<string, unknown>) => ({
    event_type: "agent_message",
    project_id: "p1",
    payload: { agent: "developer", text: "oi", ...payload },
  });

  it("aceita o payload antigo (sem os campos novos)", () => {
    expect(parseApiEvent(envelope({}))).not.toBeNull();
  });

  it("aceita task_id, attempt e seq nulos ou preenchidos", () => {
    expect(parseApiEvent(envelope({ task_id: null, attempt: null, seq: null }))).not.toBeNull();
    expect(parseApiEvent(envelope({ task_id: "T02", attempt: 2, seq: SEQ }))).not.toBeNull();
  });

  it.each([
    ["task_id numérico", { task_id: 2 }, "task_id"],
    ["attempt texto", { attempt: "2" }, "attempt"],
    ["seq texto", { seq: "7" }, "seq"],
    ["seq negativo", { seq: -1 }, "seq"],
  ])("tolera tipo errado (%s): a mensagem fica e o campo inválido vira ausente", (_label, patch, field) => {
    const parsed = parseApiEvent(envelope(patch));

    expect(parsed).not.toBeNull();
    expect(parsed?.event_type).toBe("agent_message");
    expect(parsed?.payload).toMatchObject({ text: "oi" });
    expect(parsed?.payload).not.toHaveProperty(field);
  });

  it("mantém task_id, attempt e seq válidos", () => {
    const parsed = parseApiEvent(envelope({ task_id: "T02", attempt: 2, seq: SEQ }));

    expect(parsed?.payload).toMatchObject({ task_id: "T02", attempt: 2, seq: SEQ });
  });

  it("agent e text inválidos continuam rejeitando o evento", () => {
    expect(parseApiEvent(envelope({ agent: 3 }))).toBeNull();
    expect(parseApiEvent(envelope({ text: null }))).toBeNull();
  });
});
