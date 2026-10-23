/* Folha de ações de uma tarefa no Quadro (spec 086/T05): paginação, vazio, falha e o contrato. */
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { getToolActions } from "../../api/client";
import { installFetch } from "../../test-utils";
import { formatClock, ToolActionsSheet } from "./ToolActionsSheet";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const TS = 1_760_000_000;

function item(seq: number, overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return { seq, ts: TS + seq, agent: "developer", task_id: "T01", kind: "tool", text: `read_file src/a${seq}.rs`, ...overrides };
}

describe("ToolActionsSheet", () => {
  it("lista cronológica; 'Carregar anteriores' pagina para trás e o foco vai para a lista; Esc fecha", async () => {
    const mock = installFetch((_m, url) => {
      if (url.includes("before=3")) return { body: { items: [item(1), item(2)], total: 4, has_more: false } };
      return { body: { items: [item(3, { agent: "reviewer", kind: "command", text: "npm test" }), item(4)], total: 4, has_more: true } };
    });
    const onClose = vi.fn();
    render(<ToolActionsSheet projectId="p1" specId="s1" task="T01" title="Ações da tarefa T01" onClose={onClose} />);
    const sheet = screen.getByRole("dialog", { name: "Ações da tarefa T01" });
    const list = await within(sheet).findByRole("list", { name: "Ações em ordem cronológica" });
    expect(mock.mock.calls[0]?.[0]).toBe("/api/projects/p1/specs/s1/tools?task=T01&limit=100");
    let rows = within(list).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("data-seq"))).toEqual(["3", "4"]);
    expect(rows[0]?.textContent).toContain(formatClock(TS + 3));
    expect(rows[0]?.textContent).toContain("Revisor");
    expect(within(sheet).getByText("2 de 4 ações")).toBeTruthy();

    await act(async () => {
      fireEvent.click(within(sheet).getByRole("button", { name: "Carregar anteriores" }));
    });
    await waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(4));
    expect(mock.mock.calls[1]?.[0]).toBe("/api/projects/p1/specs/s1/tools?task=T01&before=3&limit=100");
    rows = within(list).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("data-seq"))).toEqual(["1", "2", "3", "4"]);
    expect(within(sheet).queryByRole("button", { name: "Carregar anteriores" })).toBeNull();
    expect(document.activeElement).toBe(list);

    fireEvent.keyDown(sheet, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });

  it("falha oferece nova tentativa; lista vazia mostra o aviso", async () => {
    let calls = 0;
    installFetch(() => {
      calls += 1;
      return calls === 1 ? { status: 500, body: { error: "falha interna", code: "internal" } } : { body: { items: [], total: 0, has_more: false } };
    });
    render(<ToolActionsSheet projectId="p1" specId="s1" task="FINAL" title="Ações da verificação final" onClose={() => undefined} />);
    const alert = await screen.findByRole("alert");
    await act(async () => {
      fireEvent.click(within(alert).getByRole("button", { name: "Tentar de novo" }));
    });
    expect(await screen.findByText("Nenhuma ação registrada.")).toBeTruthy();
    expect(calls).toBe(2);
  });

  it("item inválido no corpo vira falha (nada é exibido do corpo)", async () => {
    installFetch(() => ({ body: { items: [{ seq: 1 }], total: 1, has_more: false } }));
    render(<ToolActionsSheet projectId="p1" specId="s1" task="T01" title="Ações" onClose={() => undefined} />);
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(screen.queryByRole("list")).toBeNull();
  });
});

describe("getToolActions (cliente)", () => {
  it("valida o formato: aceita o estágio da verificação final e rejeita item inválido", async () => {
    installFetch(() => ({ body: { items: [item(1, { stage: "final_verification", task_id: "FINAL" })], total: 1, has_more: false } }));
    const ok = await getToolActions("p1", "s1", { task: "FINAL" });
    expect(ok).toEqual({
      ok: true,
      data: { items: [{ ...item(1, { task_id: "FINAL" }), stage: "final_verification" }], total: 1, has_more: false },
    });
    installFetch(() => ({ body: { items: [{ seq: 1 }], total: 1, has_more: false } }));
    expect((await getToolActions("p1", "s1")).ok).toBe(false);
  });
});
