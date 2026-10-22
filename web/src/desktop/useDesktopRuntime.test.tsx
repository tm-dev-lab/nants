/*
 * Serviços da aba montados pelo desktop (spec 093, decisão 7): uma conexão SSE no fluxo global,
 * a classe da aba oculta (`pageVisibility`), a configuração (`configError` com texto fixo) e o
 * `project_removed` indo para a remoção do espaço.
 */
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { subscribeSpaceRemovals } from "../apps/project/projectRemoval";
import { PAGE_HIDDEN_CLASS } from "../pageVisibility";
import { appStore } from "../store";
import { FakeEventSource, installFakeEventSource, installFetch, makeConfig, makeProject, resetStore } from "../test-utils";
import { useDesktopRuntime } from "./useDesktopRuntime";

beforeEach(() => {
  installFakeEventSource();
  resetStore();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  document.documentElement.classList.remove(PAGE_HIDDEN_CLASS);
});

/** Servidor mínimo: config com dois projetos, atenção vazia e o resto vazio. */
function serve(): void {
  installFetch((_method, url) => {
    if (url === "/api/config") return { body: makeConfig([makeProject("p1", "Loja"), makeProject("p2", "Blog")]) };
    if (url === "/api/attention") return { body: { setup_incomplete: false, projects: [] } };
    return { status: 404, body: { error: "não encontrado" } };
  });
}

function setHidden(hidden: boolean): void {
  Object.defineProperty(document, "hidden", { configurable: true, value: hidden });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("serviços da aba", () => {
  it("abre uma única conexão no fluxo global e a fecha ao desmontar", async () => {
    serve();
    const { rerender, unmount } = renderHook(() => useDesktopRuntime({ attentionIntervalMs: 0 }));
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    rerender();
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.instances[0]?.url).toBe("/api/events");
    unmount();
    expect(FakeEventSource.instances[0]?.readyState).toBe(2);
  });

  it("falha da configuração vira configError com texto fixo do cliente", async () => {
    installFetch((_method, url) => (url === "/api/config" ? { status: 500, body: { error: "corpo cru do servidor" } } : { body: {} }));
    renderHook(() => useDesktopRuntime({ attentionIntervalMs: 0 }));
    await waitFor(() => expect(appStore.getState().configError).not.toBeNull());
    expect(appStore.getState().configError).not.toContain("corpo cru");
  });

  it("a aba oculta ganha a classe que pausa as animações; ao desmontar, a classe sai", async () => {
    serve();
    const { unmount } = renderHook(() => useDesktopRuntime({ attentionIntervalMs: 0 }));
    act(() => setHidden(true));
    expect(document.documentElement.classList.contains(PAGE_HIDDEN_CLASS)).toBe(true);
    act(() => setHidden(false));
    expect(document.documentElement.classList.contains(PAGE_HIDDEN_CLASS)).toBe(false);
    act(() => setHidden(true));
    unmount();
    expect(document.documentElement.classList.contains(PAGE_HIDDEN_CLASS)).toBe(false);
    setHidden(false);
  });

  it("project_removed vai para a remoção do espaço (o store esquece o projeto e o shell é avisado)", async () => {
    resetStore({ config: makeConfig([makeProject("p1", "Loja"), makeProject("p2", "Blog")]) });
    serve();
    const removed: string[] = [];
    const unsubscribe = subscribeSpaceRemovals((removal) => removed.push(removal.projectId));
    renderHook(() => useDesktopRuntime({ attentionIntervalMs: 0 }));
    await waitFor(() => expect(FakeEventSource.instances).toHaveLength(1));
    act(() => FakeEventSource.instances[0]?.emit("project_removed", { event_type: "project_removed", project_id: "p1", payload: { project_id: "p1" } }));
    expect(removed).toEqual(["p1"]);
    expect(appStore.getState().removedProjectIds.p1).toBe(true);
    unsubscribe();
  });
});
