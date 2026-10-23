import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { appStore } from "./store";
import type { ApiEvent } from "./api/types";
import { handleLiveEvent, useLiveStream, type LiveStreamHooks } from "./liveStream";
import {
  FakeEventSource,
  installFakeEventSource,
  installFetch,
  makeConfig,
  makeProject,
  makeProjectState,
  resetStore,
} from "./test-utils";

/** Assinatura global montada como no desktop (`useDesktopRuntime`). */
function GlobalStream(hooks: LiveStreamHooks): null {
  useLiveStream(hooks);
  return null;
}

const NAMES: Record<string, string> = { p1: "api-pagamentos", p2: "motor-rust" };

function permissionEvent(projectId: string, requestId: string, command: string): ApiEvent {
  return {
    event_type: "permission_requested",
    project_id: projectId,
    payload: { request_id: requestId, project_id: projectId, task_id: null, agent: "architect", command },
  };
}

function agentMessage(projectId: string, text: string): ApiEvent {
  return { event_type: "agent_message", project_id: projectId, payload: { agent: "developer", text } };
}

/**
 * Config + estado para p1/p2 (planejamento, sem specs). `serverPending` é o que o servidor
 * diz pendente: um pedido só existe no GET se o teste o puser aqui (como o servidor faria).
 */
function mockServer(serverPending: { request_id: string; agent: string; command: string; task_id: null }[] = []) {
  return installFetch((_method, url) => {
    if (url === "/api/config") {
      return { body: makeConfig([makeProject("p1", NAMES.p1 ?? ""), makeProject("p2", NAMES.p2 ?? "")]) };
    }
    const match = /^\/api\/projects\/([^/]+)\/state$/.exec(url);
    const id = match?.[1];
    if (id !== undefined && NAMES[id] !== undefined) {
      return { body: makeProjectState({ id, name: NAMES[id], pending_permissions: serverPending }) };
    }
    return { status: 404, body: { error: "projeto não encontrado", code: "project_not_found" } };
  });
}

async function openStream(): Promise<FakeEventSource> {
  await waitFor(() => expect(FakeEventSource.instances.length).toBeGreaterThan(0));
  const source = FakeEventSource.instances[0];
  if (source === undefined) throw new Error("stream ausente");
  return source;
}

beforeEach(() => {
  installFakeEventSource();
  resetStore();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("assinatura global do stream", () => {
  it("uma única conexão enquanto o chamador vive; re-render não duplica conexão nem mensagens", async () => {
    mockServer();
    const { rerender } = render(<GlobalStream />);
    const source = await openStream();
    act(() => source.emit("agent_message", agentMessage("p1", "Mensagem única")));
    for (let i = 0; i < 3; i += 1) rerender(<GlobalStream />);

    expect(FakeEventSource.instances).toHaveLength(1);
    expect(appStore.getState().messages.p1?.filter((m) => m.text === "Mensagem única")).toHaveLength(1);
  });

  it("fecha a conexão ao desmontar", async () => {
    mockServer();
    const { unmount } = render(<GlobalStream />);
    const source = await openStream();
    expect(source.readyState).not.toBe(2);

    unmount();

    expect(source.readyState).toBe(2);
  });

  it("reconexão preserva as filas de permissão", async () => {
    mockServer([{ request_id: "r1", agent: "architect", command: "cargo test", task_id: null }]);
    render(<GlobalStream />);
    const source = await openStream();
    act(() => source.emit("permission_requested", permissionEvent("p1", "r1", "cargo test")));
    expect(appStore.getState().permissions.p1?.map((p) => p.requestId)).toEqual(["r1"]);

    act(() => source.onerror?.(new Event("error")));

    await waitFor(() => expect(FakeEventSource.instances.length).toBe(2), { timeout: 4000 });
    expect(appStore.getState().permissions.p1?.map((p) => p.requestId)).toEqual(["r1"]);
  });

  it("os ganchos recebem o evento no lugar do store e são chamados depois do lagged", async () => {
    mockServer();
    const seen: string[] = [];
    let lagged = 0;
    render(<GlobalStream onEvent={(event) => seen.push(event.event_type)} onLagged={() => (lagged += 1)} />);
    const source = await openStream();
    act(() => source.emit("agent_message", agentMessage("p1", "Só no gancho")));
    act(() => source.emit("lagged", { skipped: 3 }));

    expect(seen).toEqual(["agent_message"]);
    expect(appStore.getState().messages.p1 ?? []).toEqual([]);
    expect(lagged).toBe(1);
  });
});

describe("handleLiveEvent: isolamento e limites", () => {
  it("eventos de dois projetos não se misturam", () => {
    handleLiveEvent(permissionEvent("p1", "r1", "a"));
    handleLiveEvent(permissionEvent("p2", "r2", "b"));
    handleLiveEvent(agentMessage("p2", "só p2"));

    const state = appStore.getState();
    expect(state.permissions.p1?.map((p) => p.requestId)).toEqual(["r1"]);
    expect(state.permissions.p2?.map((p) => p.requestId)).toEqual(["r2"]);
    expect(state.messages.p1).toBeUndefined();
    expect(state.messages.p2?.map((m) => m.text)).toEqual(["só p2"]);
  });

  it("limita a 100 projetos com fila, descartando os mais antigos", () => {
    for (let i = 0; i < 101; i += 1) {
      handleLiveEvent(permissionEvent(`proj-${i}`, `r-${i}`, "cmd"));
    }
    const keys = Object.keys(appStore.getState().permissions);
    expect(keys).toHaveLength(100);
    expect(keys).not.toContain("proj-0");
    expect(keys).toContain("proj-100");
  });

  it("task_updated incrementa só a revisão do projeto afetado", () => {
    handleLiveEvent({
      event_type: "task_updated",
      project_id: "p1",
      payload: { spec_id: "s1", task_id: "T01", status: "DONE" },
    });
    expect(appStore.getState().revisions).toEqual({ p1: 1 });
  });
});
