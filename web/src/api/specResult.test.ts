import { afterEach, describe, expect, it, vi } from "vitest";
import { getSpecResult } from "./client";
import { installFetch } from "../test-utils";

/** JSON literal no formato real de `GET /api/projects/:id/specs/:spec_id/result` (spec_result.rs). */
const SERVER_RESULT = {
  spec_id: "001-x",
  title: "Api de pagamentos",
  status: "COMPLETED",
  tasks: [
    { id: "T01", title: "Modelo", status: "DONE" },
    { id: "T02", title: "Rotas", status: "DONE", error: "arquivo inválido" },
  ],
  commits: [
    {
      hash: "8d0d36f",
      subject: "feat(001-x): T03 - Rotas",
      task_id: "T03",
      date: "2026-10-14T13:05:00Z",
    },
  ],
  files: [
    { path: "src/app.js", change: "A" },
    { path: "README.md", change: "M" },
    { path: "old.js", change: "D" },
  ],
  files_truncated: false,
  readme: { text: "# Projeto\n\nRode `npm test`.", truncated: false },
  project_path: "/home/user/projetos/api",
  test_command: "node --test",
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("getSpecResult: contrato", () => {
  it("faz GET no endpoint da spec com ids codificados e devolve o resultado tipado", async () => {
    const fetchMock = installFetch(() => ({ body: SERVER_RESULT }));
    const result = await getSpecResult("p 1", "001-x");
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/projects/p%201/specs/001-x/result");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.data.title).toBe("Api de pagamentos");
    expect(result.data.commits[0]?.hash).toBe("8d0d36f");
    expect(result.data.files.map((f) => f.change)).toEqual(["A", "M", "D"]);
    expect(result.data.readme?.truncated).toBe(false);
    expect(result.data.test_command).toBe("node --test");
  });

  it("aceita readme null, test_command null e campos truncados", async () => {
    installFetch(() => ({
      body: { ...SERVER_RESULT, readme: null, test_command: null, files_truncated: true },
    }));
    const result = await getSpecResult("p1", "001-x");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.data.readme).toBeNull();
    expect(result.data.test_command).toBeNull();
    expect(result.data.files_truncated).toBe(true);
  });

  it("corpo com campo ausente vira invalid_response (sem quebrar)", async () => {
    const { title: _title, ...semTitulo } = SERVER_RESULT;
    installFetch(() => ({ body: semTitulo }));
    const result = await getSpecResult("p1", "001-x");
    expect(result).toMatchObject({ ok: false, error: { kind: "invalid_response" } });
  });

  it("tipos errados viram invalid_response: tasks não-lista, change desconhecido, readme malformado", async () => {
    const cases: unknown[] = [
      { ...SERVER_RESULT, tasks: "T01" },
      { ...SERVER_RESULT, files: [{ path: "a.js", change: "X" }] },
      { ...SERVER_RESULT, readme: { text: 42, truncated: false } },
      { ...SERVER_RESULT, commits: [{ hash: "a" }] },
      { ...SERVER_RESULT, files_truncated: "sim" },
    ];
    for (const body of cases) {
      installFetch(() => ({ body }));
      const result = await getSpecResult("p1", "001-x");
      expect(result).toMatchObject({ ok: false, error: { kind: "invalid_response" } });
    }
  });

  it("404 vira falha estática do cliente, sem expor o corpo", async () => {
    installFetch(() => ({ status: 404, body: { error: "spec não encontrada", leak: "<b>x</b>" } }));
    const result = await getSpecResult("p1", "001-x");
    expect(result).toMatchObject({ ok: false, error: { kind: "http", status: 404 } });
    expect(JSON.stringify(result)).not.toContain("leak");
  });

  it("falha de rede vira network", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("offline"))));
    const result = await getSpecResult("p1", "001-x");
    expect(result).toMatchObject({ ok: false, error: { kind: "network" } });
  });
});
