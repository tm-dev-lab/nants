import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { loadDialog, refreshDialogs, resetDialogHistory, DIALOG_MAX_MESSAGES, DIALOG_PAGE_LIMIT } from "./dialogHistory";
import { appStore } from "./store";
import type { DialogMessage, ProjectState } from "./api/types";
import { installFetch, makeConfig, makeProject, makeProjectState, makeSpec, makeTask, resetStore } from "./test-utils";

const PID = "p1";
const SPEC = "s1";
const BASE = 1_700_000_000_000_000;

/** Mensagens do servidor `seq = BASE + i` (i de 1 a `total`), no formato do GET. */
function rowsUpTo(total: number, after: number, limit: number): DialogMessage[] {
  const out: DialogMessage[] = [];
  for (let i = 1; i <= total && out.length < limit; i += 1) {
    const seq = BASE + i;
    if (seq > after) out.push({ seq, ts: 1_700_000_000 + i, agent: "developer", task_id: "T02", attempt: 1, text: `r${i}` });
  }
  return out;
}

/** Servidor simulado: responde `GET .../dialog?after&limit` com `total` mensagens. */
function serveDialog(total: () => number) {
  return installFetch((_method, url) => {
    if (url.endsWith("/state")) return { body: stateWithSpec() };
    if (!url.includes("/dialog")) return { status: 404, body: { error: "projeto não encontrado", code: "project_not_found" } };
    const query = new URL(url, "http://local").searchParams;
    const after = Number(query.get("after") ?? "0");
    const limit = Number(query.get("limit") ?? "200");
    const n = total();
    return {
      body: { messages: rowsUpTo(n, after, limit), last_seq: BASE + n, truncated_before: false },
    };
  });
}

function stateWithSpec(specId: string = SPEC): ProjectState {
  return makeProjectState({
    id: PID,
    specs: [makeSpec(specId, "Plano", "IN_PROGRESS", [makeTask("T02", "Middleware", "IN_PROGRESS")])],
  });
}

function dialogUrls(mock: { mock: { calls: unknown[][] } }): string[] {
  return mock.mock.calls.map((c) => String(c[0])).filter((u) => u.includes("/dialog"));
}

function agentTexts(): string[] {
  return (appStore.getState().messages[PID] ?? []).filter((m) => m.role === "agent").map((m) => m.text);
}

beforeEach(() => {
  resetDialogHistory();
  resetStore({
    config: makeConfig([makeProject(PID, "api-pagamentos")]),
    projectStates: { [PID]: stateWithSpec() },
  });
});

afterEach(() => {
  resetDialogHistory();
});

describe("loadDialog: paginação", () => {
  it("pagina em ordem crescente até a página vir menor que o limite", async () => {
    const mock = serveDialog(() => 1100);

    const outcome = await loadDialog(PID, SPEC);

    expect(outcome).toBe("applied");
    // O store guarda até MAX_MESSAGES_PER_PROJECT (500) mensagens: ficam as 500 mais recentes.
    expect(agentTexts()).toHaveLength(500);
    expect(agentTexts()[0]).toBe("r601");
    expect(agentTexts()[499]).toBe("r1100");
    const dialogCalls = mock.mock.calls.map((c) => String(c[0])).filter((u) => u.includes("/dialog"));
    expect(dialogCalls).toHaveLength(3);
    expect(dialogCalls[0]).toContain(`limit=${DIALOG_PAGE_LIMIT}`);
  });

  it("com exatamente um limite cheio, faz a página vazia seguinte e para", async () => {
    const mock = serveDialog(() => DIALOG_PAGE_LIMIT);

    await loadDialog(PID, SPEC);

    expect(agentTexts()).toHaveLength(DIALOG_PAGE_LIMIT);
    expect(mock.mock.calls.filter((c) => String(c[0]).includes("/dialog"))).toHaveLength(2);
  });

  it("acima do teto de carga lê até 2000 e fica com as mais recentes (store: 500)", async () => {
    const total = DIALOG_MAX_MESSAGES + 600;
    const mock = serveDialog(() => total);

    await loadDialog(PID, SPEC);

    const dialogCalls = mock.mock.calls.map((c) => String(c[0])).filter((u) => u.includes("/dialog"));
    // 2600 mensagens lidas em páginas de 500 (a janela guarda só as 2000 mais recentes).
    expect(dialogCalls).toHaveLength(6);
    const texts = agentTexts();
    expect(texts).toHaveLength(500);
    expect(texts[0]).toBe(`r${total - 499}`);
    expect(texts.at(-1)).toBe(`r${total}`);
  });

  it("falha do GET não altera o que já está em memória", async () => {
    serveDialog(() => 3);
    await loadDialog(PID, SPEC);
    expect(agentTexts()).toEqual(["r1", "r2", "r3"]);

    installFetch(() => ({ status: 500, body: { error: "falha interna", code: "internal" } }));
    const outcome = await loadDialog(PID, SPEC);

    expect(outcome).toBe("failed");
    expect(agentTexts()).toEqual(["r1", "r2", "r3"]);
  });
});

describe("loadDialog: respostas atrasadas", () => {
  it("só a carga mais nova do mesmo projeto/spec é aplicada", async () => {
    const gate: { release?: () => void } = {};
    let calls = 0;
    installFetch(async (_m, url) => {
      if (url.endsWith("/state")) return { body: stateWithSpec() };
      calls += 1;
      if (calls === 1) {
        await new Promise<void>((resolve) => {
          gate.release = resolve;
        });
        return { body: { messages: rowsUpTo(1, 0, 500).map((m) => ({ ...m, text: "antiga" })), last_seq: BASE + 1, truncated_before: false } };
      }
      return { body: { messages: rowsUpTo(2, 0, 500), last_seq: BASE + 2, truncated_before: false } };
    });

    const first = loadDialog(PID, SPEC);
    const second = await loadDialog(PID, SPEC);
    gate.release?.();
    const firstOutcome = await first;

    expect(second).toBe("applied");
    expect(firstOutcome).toBe("stale");
    expect(agentTexts()).toEqual(["r1", "r2"]);
  });

  it("projeto removido durante a carga descarta a resposta", async () => {
    serveDialog(() => 2);
    const pending = loadDialog(PID, SPEC);
    appStore.forgetProject(PID);

    expect(await pending).toBe("stale");
    expect(appStore.getState().messages[PID]).toBeUndefined();
  });
});

describe("cargas por spec: identidade e ordem", () => {
  it("carga da spec A em voo, B resolve antes e A depois: permanece B", async () => {
    const gate: { release?: () => void } = {};
    installFetch(async (_m, url) => {
      if (url.endsWith("/state")) return { body: stateWithSpec("s2") };
      if (url.includes("/specs/s1/")) {
        await new Promise<void>((resolve) => {
          gate.release = resolve;
        });
        return { body: { messages: rowsUpTo(2, 0, 500).map((m) => ({ ...m, text: "A" })), last_seq: BASE + 2, truncated_before: false } };
      }
      return { body: { messages: rowsUpTo(1, 0, 500).map((m) => ({ ...m, text: "B" })), last_seq: BASE + 1, truncated_before: false } };
    });
    resetStore({ config: makeConfig([makeProject(PID, "api-pagamentos")]), projectStates: { [PID]: stateWithSpec("s1") } });
    const a = loadDialog(PID, "s1");
    appStore.setState((s) => ({ projectStates: { ...s.projectStates, [PID]: stateWithSpec("s2") } }));

    const b = await loadDialog(PID, "s2");
    gate.release?.();
    const outcomeA = await a;

    expect(b).toBe("applied");
    expect(outcomeA).toBe("stale");
    expect(agentTexts()).toEqual(["B"]);
  });

  it("reconexão durante a carga inicial é aplicada depois: busca after=<maior seq> ao aplicar", async () => {
    const gate: { release?: () => void } = {};
    const mock = installFetch(async (_m, url) => {
      if (url.endsWith("/state")) return { body: stateWithSpec() };
      const after = Number(new URL(url, "http://local").searchParams.get("after") ?? "0");
      if (after === 0) {
        await new Promise<void>((resolve) => {
          gate.release = resolve;
        });
        return { body: { messages: rowsUpTo(3, 0, 500), last_seq: BASE + 3, truncated_before: false } };
      }
      return { body: { messages: rowsUpTo(5, after, 500), last_seq: BASE + 5, truncated_before: false } };
    });

    const pending = loadDialog(PID, SPEC);
    const during = refreshDialogs();
    gate.release?.();
    expect(await pending).toBe("applied");
    await during;

    expect(dialogUrls(mock).some((u) => u.includes(`after=${BASE + 3}`))).toBe(true);
    expect(agentTexts()).toEqual(["r1", "r2", "r3", "r4", "r5"]);
  });

  it("carga falha; a reconexão seguinte faz a carga completa", async () => {
    let failing = true;
    const mock = installFetch((_m, url) => {
      if (url.endsWith("/state")) return { body: stateWithSpec() };
      if (failing) return { status: 500, body: { error: "falha interna", code: "internal" } };
      return { body: { messages: rowsUpTo(2, 0, 500), last_seq: BASE + 2, truncated_before: false } };
    });
    expect(await loadDialog(PID, SPEC)).toBe("failed");

    failing = false;
    await refreshDialogs();

    expect(dialogUrls(mock).some((u) => !u.includes("after="))).toBe(true);
    expect(agentTexts()).toEqual(["r1", "r2"]);
  });
});

describe("cargas superadas do mesmo slot não apagam a recuperação pendente", () => {
  it("A#1 em voo, A#2 iniciada, recuperação durante A#2, A#1 resolve primeiro: A#2 aplica e recupera", async () => {
    const gates: Array<() => void> = [];
    let fullCalls = 0;
    const mock = installFetch(async (_m, url) => {
      if (url.endsWith("/state")) return { body: stateWithSpec() };
      const hasAfter = url.includes("after=");
      if (!hasAfter) {
        fullCalls += 1;
        const n = fullCalls;
        await new Promise<void>((resolve) => {
          gates[n - 1] = resolve;
        });
        return { body: { messages: rowsUpTo(3, 0, 500), last_seq: BASE + 3, truncated_before: false } };
      }
      const after = Number(new URL(url, "http://local").searchParams.get("after"));
      return { body: { messages: rowsUpTo(5, after, 500), last_seq: BASE + 5, truncated_before: false } };
    });

    const first = loadDialog(PID, SPEC);
    const second = loadDialog(PID, SPEC);
    const during = refreshDialogs();
    gates[0]?.();
    expect(await first).toBe("stale");
    gates[1]?.();
    expect(await second).toBe("applied");
    await during;

    expect(dialogUrls(mock).some((u) => u.includes(`after=${BASE + 3}`))).toBe(true);
    expect(agentTexts()).toEqual(["r1", "r2", "r3", "r4", "r5"]);
  });
});

describe("refreshDialogs: recuperação após reconexão ou lagged", () => {
  it("busca after=<maior seq conhecido> e mescla sem duplicar", async () => {
    let total = 3;
    const mock = serveDialog(() => total);
    await loadDialog(PID, SPEC);
    total = 5;
    mock.mockClear();

    await refreshDialogs();

    const urls = mock.mock.calls.map((c) => String(c[0])).filter((u) => u.includes("/dialog"));
    expect(urls).toHaveLength(1);
    expect(urls[0]).toContain(`after=${BASE + 3}`);
    expect(agentTexts()).toEqual(["r1", "r2", "r3", "r4", "r5"]);
  });

  it("falha na recuperação mantém o diálogo que já estava na tela", async () => {
    serveDialog(() => 2);
    await loadDialog(PID, SPEC);
    installFetch(() => ({ status: 500, body: { error: "falha interna", code: "internal" } }));

    await refreshDialogs();

    expect(agentTexts()).toEqual(["r1", "r2"]);
  });

  it("não faz nada para projeto cujo diálogo nunca foi carregado", async () => {
    const mock = serveDialog(() => 2);

    await refreshDialogs();

    expect(mock.mock.calls.filter((c) => String(c[0]).includes("/dialog"))).toHaveLength(0);
  });
});
