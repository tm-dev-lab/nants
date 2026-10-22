import { afterEach, describe, expect, it, vi } from "vitest";
import { getSpecReport } from "./client";
import { parseApiEvent } from "./events";
import { installFetch } from "../test-utils";

afterEach(() => {
  vi.unstubAllGlobals();
});

const REPORT = { markdown: "# Relatório\n\n## Como executar\n\n1. npm start", generated_at: "2026-10-14T13:05:00Z", source: "architect" };

describe("getSpecReport", () => {
  it("GET do relatório devolve markdown, data e origem", async () => {
    const fetchMock = installFetch(() => ({ body: REPORT }));
    const result = await getSpecReport("p1", "001-x");
    expect(result).toEqual({ ok: true, data: REPORT });
    expect(String(fetchMock.mock.calls[0]?.[0])).toBe("/api/projects/p1/specs/001-x/report");
  });

  it("404 é falha de HTTP (sem relatório), não resposta válida", async () => {
    installFetch(() => ({ status: 404, body: { error: "relatório não encontrado" } }));
    const result = await getSpecReport("p1", "001-x");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error.status).toBe(404);
  });

  it("corpo sem markdown textual é tratado como resposta inesperada", async () => {
    installFetch(() => ({ body: { markdown: 42, generated_at: "x", source: "architect" } }));
    const result = await getSpecReport("p1", "001-x");
    expect(result.ok).toBe(false);
  });
});

describe("eventos report_ready e report_failed", () => {
  it("report_ready exige spec_id texto", () => {
    expect(parseApiEvent({ event_type: "report_ready", project_id: "p1", payload: { spec_id: "001-x" } })).not.toBeNull();
    expect(parseApiEvent({ event_type: "report_ready", project_id: "p1", payload: {} })).toBeNull();
  });

  it("report_failed exige spec_id e code texto", () => {
    const ok = parseApiEvent({
      event_type: "report_failed",
      project_id: "p1",
      payload: { spec_id: "001-x", code: "git_unsupported" },
    });
    expect(ok).not.toBeNull();
    expect(
      parseApiEvent({ event_type: "report_failed", project_id: "p1", payload: { spec_id: "001-x" } }),
    ).toBeNull();
  });
});
