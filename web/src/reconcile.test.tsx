import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { useLiveStream } from "./liveStream";
import { refreshProjectState } from "./projectState";
import { appStore } from "./store";
import type { ApiEvent } from "./api/types";
import {
  FakeEventSource,
  installFakeEventSource,
  installFetch,
  makeConfig,
  makeProject,
  makeProjectState,
  resetStore,
  type RouteReply,
} from "./test-utils";

/** Assinatura global montada como no desktop (`useDesktopRuntime`). */
function GlobalStream(): null {
  useLiveStream();
  return null;
}

const PID = "p1";

function permissionEvent(requestId: string, command: string): ApiEvent {
  return {
    event_type: "permission_requested",
    project_id: PID,
    payload: { request_id: requestId, project_id: PID, task_id: null, agent: "architect", command },
  };
}

function ids(): string[] {
  return (appStore.getState().permissions[PID] ?? []).map((p) => p.requestId);
}

async function openStream(): Promise<FakeEventSource> {
  await waitFor(() => expect(FakeEventSource.instances.length).toBeGreaterThan(0));
  const source = FakeEventSource.instances[0];
  if (source === undefined) throw new Error("stream ausente");
  return source;
}

/**
 * Rotas do projeto: GET de estado (com `pending` do servidor) controlado pelo teste.
 * `stateGet(n)` recebe o número da chamada (1 = primeira) e pode devolver promessa.
 */
function stubRoutes(opts: {
  pending?: { request_id: string; agent: string; command: string; task_id: null }[];
  stateGet?: (n: number) => Promise<RouteReply> | RouteReply;
}) {
  let stateCalls = 0;
  const mock = vi.fn(async (input: string | URL | Request) => {
    const url = String(input);
    if (url.endsWith("/state")) {
      stateCalls += 1;
      const reply = opts.stateGet
        ? await opts.stateGet(stateCalls)
        : { body: makeProjectState({ pending_permissions: opts.pending ?? [] }) };
      return new Response(JSON.stringify(reply.body ?? null), {
        status: reply.status ?? 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    return new Response(JSON.stringify({ error: "projeto não encontrado", code: "project_not_found" }), { status: 404 });
  });
  vi.stubGlobal("fetch", mock);
  return { mock, stateCalls: () => stateCalls };
}

beforeEach(() => {
  installFakeEventSource();
  resetStore({ config: makeConfig([makeProject(PID, "api-pagamentos")]) });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

// A Conversa relê o estado do projeto (`refreshProjectState`) depois de todo POST do chat, com
// sucesso ou falha (ver `Conversation.turn.test`). Aqui, o efeito dessa releitura no store.
describe("reconciliação após turno do chat que falhou (F-07)", () => {
  it("GET sem o pedido pendente: fila vazia e fase liberada", async () => {
    stubRoutes({});
    render(<GlobalStream />);
    const source = await openStream();
    act(() => source.emit("permission_requested", permissionEvent("r1", "git log")));
    expect(ids()).toEqual(["r1"]);

    await act(async () => {
      await refreshProjectState(PID);
    });

    expect(ids()).toEqual([]);
    expect(appStore.getState().phases[PID]).toBeUndefined();
  });

  it("pedido novo por SSE durante o GET de reconciliação é preservado", async () => {
    let releaseGet: ((r: RouteReply) => void) | undefined;
    stubRoutes({ stateGet: () => new Promise<RouteReply>((resolve) => (releaseGet = resolve)) });
    render(<GlobalStream />);
    const source = await openStream();
    act(() => source.emit("permission_requested", permissionEvent("r-old", "git log")));
    const pending = refreshProjectState(PID);
    await waitFor(() => expect(releaseGet).toBeDefined());

    act(() => source.emit("permission_requested", permissionEvent("r-new", "cargo test")));
    releaseGet?.({ body: makeProjectState({ pending_permissions: [] }) });
    await act(async () => {
      await pending;
    });

    expect(ids()).toEqual(["r-new"]);
  });
});

describe("reconexão do stream (F-07 item 2)", () => {
  it("reabrir depois de erro pede a releitura do estado de todos os projetos", async () => {
    stubRoutes({});
    render(<GlobalStream />);
    const first = await openStream();
    const before = appStore.getState().revisions[PID] ?? 0;

    act(() => first.onerror?.(new Event("error")));
    await waitFor(() => expect(FakeEventSource.instances.length).toBe(2), { timeout: 4000 });
    const second = FakeEventSource.instances[1];
    act(() => second?.onopen?.(new Event("open")));

    await waitFor(() => expect(appStore.getState().revisions[PID]).toBe(before + 1));
  });
});

describe("GET do estado superado (F-08)", () => {
  function deferredGets(): ((r: RouteReply) => void)[] {
    const resolvers: ((r: RouteReply) => void)[] = [];
    installFetch(
      () =>
        new Promise<RouteReply>((resolve) => {
          resolvers.push(resolve);
        }),
    );
    return resolvers;
  }

  it("GET A ok superado por GET B com falha: o estado de A é aplicado", async () => {
    resetStore();
    const resolvers = deferredGets();
    const a = refreshProjectState(PID);
    const b = refreshProjectState(PID);

    resolvers[1]?.({ status: 500, body: { error: "falha interna", code: "internal" } });
    await b;
    resolvers[0]?.({ body: makeProjectState({ name: "velho" }) });
    await a;

    expect(appStore.getState().projectStates[PID]?.name).toBe("velho");
  });

  it("GET A antigo nunca sobrescreve um estado mais novo já aplicado", async () => {
    resetStore();
    const resolvers = deferredGets();
    const a = refreshProjectState(PID);
    const b = refreshProjectState(PID);

    resolvers[1]?.({ body: makeProjectState({ name: "novo" }) });
    await b;
    resolvers[0]?.({ body: makeProjectState({ name: "velho" }) });
    await a;

    expect(appStore.getState().projectStates[PID]?.name).toBe("novo");
  });
});
