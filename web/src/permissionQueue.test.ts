import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { appStore } from "./store";
import type { ApiEvent, ProjectState } from "./api/types";
import { handleLiveEvent } from "./liveStream";
import { refreshProjectState } from "./projectState";
import { installFetch, makeProjectState, resetStore, type RouteReply } from "./test-utils";

function permission(projectId: string, requestId: string, command = "cmd"): ApiEvent {
  return {
    event_type: "permission_requested",
    project_id: projectId,
    payload: { request_id: requestId, project_id: projectId, task_id: null, agent: "developer", command },
  };
}

function ids(projectId: string): string[] {
  return (appStore.getState().permissions[projectId] ?? []).map((p) => p.requestId);
}

/** Respostas do GET controladas pelo teste, na ordem em que foram pedidas. */
function deferredState(): { fetchMock: ReturnType<typeof installFetch>; resolvers: ((r: RouteReply) => void)[] } {
  const resolvers: ((r: RouteReply) => void)[] = [];
  const fetchMock = installFetch(
    () =>
      new Promise<RouteReply>((resolve) => {
        resolvers.push(resolve);
      }),
  );
  return { fetchMock, resolvers };
}

function stateWith(overrides: Partial<ProjectState>): RouteReply {
  return { body: makeProjectState(overrides) };
}

beforeEach(() => {
  resetStore();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fila de permissões: LRU e teto de projetos", () => {
  it("ao receber pedido, o projeto vai para o fim da ordem LRU", () => {
    for (let i = 0; i < 3; i += 1) handleLiveEvent(permission(`p${i}`, `r${i}`));
    handleLiveEvent(permission("p0", "r0b"));
    expect(Object.keys(appStore.getState().permissions)).toEqual(["p1", "p2", "p0"]);
  });

  it("teto de 100: descarta a fila menos recente (LRU), não a que acabou de ser tocada", () => {
    for (let i = 0; i < 100; i += 1) handleLiveEvent(permission(`p${i}`, `r${i}`));
    handleLiveEvent(permission("p0", "r0b")); // p0 vira a mais recente
    handleLiveEvent(permission("p100", "r100"));

    const keys = Object.keys(appStore.getState().permissions);
    expect(keys).toHaveLength(100);
    expect(keys).not.toContain("p1");
    expect(keys).toContain("p0");
    expect(keys).toContain("p100");
  });

  it("teto de 100: descarta primeiro fila vazia e nunca uma com pendência enquanto houver vazia", () => {
    for (let i = 0; i < 100; i += 1) handleLiveEvent(permission(`p${i}`, `r${i}`));
    appStore.clearPermission("p5", "r5"); // fila de p5 fica vazia (ainda no mapa)
    handleLiveEvent(permission("p100", "r100"));

    const keys = Object.keys(appStore.getState().permissions);
    expect(keys).toHaveLength(100);
    expect(keys).not.toContain("p5");
    expect(keys).toContain("p0");
    expect(ids("p0")).toEqual(["r0"]);
  });
});

describe("GET do estado: sequência e hidratação", () => {
  it("respostas fora de ordem: só a mais recente é aplicada", async () => {
    const { resolvers } = deferredState();
    const first = refreshProjectState("p1");
    const second = refreshProjectState("p1");

    resolvers[1]?.(stateWith({ name: "novo" }));
    const newest = await second;
    resolvers[0]?.(stateWith({ name: "velho" }));
    const stale = await first;

    expect(newest?.ok).toBe(true);
    expect(stale).toBeNull();
    expect(appStore.getState().projectStates.p1?.name).toBe("novo");
  });

  it("pedido que chegou durante uma queda do stream aparece após o GET", async () => {
    installFetch(() => stateWith({ pending_permissions: [{ request_id: "r9", agent: "architect", command: "git status", task_id: null }] }));
    await refreshProjectState("p1");
    expect(ids("p1")).toEqual(["r9"]);
    expect(appStore.getState().permissions.p1?.[0]?.command).toBe("git status");
    expect(appStore.getState().phases.p1).toBe("waiting_approval");
  });

  it("pedido resolvido no servidor some da fila", async () => {
    handleLiveEvent(permission("p1", "r1"));
    installFetch(() => stateWith({ pending_permissions: [] }));
    await refreshProjectState("p1");
    expect(ids("p1")).toEqual([]);
    expect(appStore.getState().phases.p1).toBeUndefined();
  });

  it("merge por request_id sem duplicar", async () => {
    handleLiveEvent(permission("p1", "r1", "antigo"));
    installFetch(() =>
      stateWith({
        pending_permissions: [
          { request_id: "r1", agent: "developer", command: "antigo", task_id: null },
          { request_id: "r2", agent: "reviewer", command: "novo", task_id: "T01" },
        ],
      }),
    );
    await refreshProjectState("p1");
    expect(ids("p1")).toEqual(["r1", "r2"]);
    expect(appStore.getState().permissions.p1?.[1]?.taskId).toBe("T01");
  });

  it("pedido que chega por SSE enquanto o GET está em voo não é removido pela hidratação", async () => {
    const { resolvers } = deferredState();
    const pending = refreshProjectState("p1");
    handleLiveEvent(permission("p1", "r3"));
    resolvers[0]?.(stateWith({ pending_permissions: [] }));
    await pending;
    expect(ids("p1")).toEqual(["r3"]);
  });
});
