/*
 * Remoção no desktop e regras da janela Projeto (spec 093, T08): idempotência, remoção local em
 * voo, mensagens fixas, o destino depois da remoção, os limites e o resumo.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SpaceData } from "../../desktop/useSpaceData";
import { appStore } from "../../store";
import { makeConfig, makeProject, makeProjectState, resetStore } from "../../test-utils";
import { parseLimit, projectSummary } from "./projectModel";
import {
  beginLocalRemoval,
  endLocalRemoval,
  handleSpaceRemovedEvent,
  nextSpaceAfterRemoval,
  removalErrorMessage,
  removeSpaceFromView,
  subscribeSpaceRemovals,
  type SpaceRemoval,
} from "./projectRemoval";

let received: SpaceRemoval[] = [];
let unsubscribe: () => void = () => undefined;

beforeEach(() => {
  resetStore({ config: makeConfig([makeProject("p1", "Loja"), makeProject("p2", "Blog")]) });
  received = [];
  unsubscribe = subscribeSpaceRemovals((r) => received.push(r));
});

afterEach(() => {
  unsubscribe();
  endLocalRemoval("p1");
});

describe("removeSpaceFromView", () => {
  it("tira o projeto do store e avisa uma vez, com o nome; a segunda chamada não avisa", () => {
    removeSpaceFromView("p1", { remote: true });
    expect(received).toEqual([{ projectId: "p1", name: "Loja", remote: true }]);
    expect(appStore.getState().config?.projects.map((p) => p.id)).toEqual(["p2"]);
    expect(appStore.getState().removedProjectIds.p1).toBe(true);
    removeSpaceFromView("p1", { remote: true });
    handleSpaceRemovedEvent("p1");
    expect(received).toHaveLength(1);
  });

  it("evento SSE: remoto por padrão; com a remoção local em voo, não é de outro cliente", () => {
    beginLocalRemoval("p1");
    handleSpaceRemovedEvent("p1");
    expect(received[0]?.remote).toBe(false);
    handleSpaceRemovedEvent("p2");
    expect(received[1]).toEqual({ projectId: "p2", name: "Blog", remote: true });
  });

  it("cancelar a assinatura para de avisar", () => {
    const other = vi.fn();
    const stop = subscribeSpaceRemovals(other);
    stop();
    removeSpaceFromView("p2", { remote: true });
    expect(other).not.toHaveBeenCalled();
    expect(received).toHaveLength(1);
  });
});

describe("regras", () => {
  it("destino: o primeiro outro projeto, ou as boas-vindas", () => {
    const projects = [makeProject("p1", "Loja"), makeProject("p2", "Blog")];
    expect(nextSpaceAfterRemoval(projects, "p1")).toBe("p2");
    expect(nextSpaceAfterRemoval(projects, "p2")).toBe("p1");
    expect(nextSpaceAfterRemoval([makeProject("p1", "Loja")], "p1")).toBeNull();
  });

  it("mensagens fixas da falha", () => {
    expect(removalErrorMessage({ kind: "http", status: 409, message: "x", code: "run_active" })).toContain("Aguarde terminar");
    expect(removalErrorMessage({ kind: "http", status: 409, message: "x", code: "chat_active" })).toContain("Aguarde a resposta");
    expect(removalErrorMessage({ kind: "network", status: 0, message: "sem conexão" })).toBe("sem conexão");
  });

  it("limites de 1 a 10", () => {
    expect(["1", "10", " 5 ", "0", "11", "a", "1.5", ""].map(parseLimit)).toEqual([1, 10, 5, null, null, null, null, null]);
  });

  it("resumo: modo, internet e tentativas", () => {
    const space = (state: ReturnType<typeof makeProjectState> | null) => ({ space: { state } as unknown as SpaceData, readiness: null, projectCount: 1 });
    expect(projectSummary({ space: null, readiness: null, projectCount: 0 })).toEqual({});
    expect(projectSummary(space(null))).toEqual({});
    expect(projectSummary(space(makeProjectState({ max_attempts: 5 }))).subtitle).toBe("modo padrão · 5 tentativas");
    expect(projectSummary(space(makeProjectState({ yolo: true, architect_internet: true, max_attempts: 1 }))).subtitle).toBe("aprovação automática · internet · 1 tentativa");
  });
});
