/*
 * Remoção do projeto no store (o espaço removido sai do desktop): `forgetProject` limpa tudo do id,
 * é idempotente, descarta eventos e respostas atrasadas do id removido, e `readmitProject` o libera
 * quando o mesmo caminho é cadastrado de novo.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { appStore } from "./store";
import { refreshProjectState } from "./projectState";
import { loadChatHistory } from "./chatHistory";
import { installFetch, makeConfig, makeProject, makeProjectState, resetStore } from "./test-utils";

afterEach(() => {
  vi.unstubAllGlobals();
});

/** Estado de um projeto com todas as fatias preenchidas. */
function seedProject(id: string): void {
  resetStore({
    config: makeConfig([makeProject(id, "api-pagamentos"), makeProject("p2", "motor-rust")]),
    projects: [{ id, name: "api-pagamentos", path: "/home/dev/api-pagamentos", created_at: "x" }],
    activeProjectId: id,
    projectStates: { [id]: makeProjectState({ id }) },
    messages: { [id]: [{ id: 1, role: "user", text: "oi", at: 1 }] },
    permissions: { [id]: [{ requestId: "r1", projectId: id, taskId: null, agent: "developer", command: "ls" }] },
    phases: { [id]: "waiting_approval" },
    revisions: { [id]: 3 },
    shownTurns: { [id]: ["t1"] },
  });
}

describe("remoção local do projeto no store", () => {
  it("forgetProject limpa todo o estado do id e preserva os demais", () => {
    seedProject("p1");

    appStore.forgetProject("p1");

    const s = appStore.getState();
    expect(s.config?.projects.map((p) => p.id)).toEqual(["p2"]);
    expect(s.projects).toEqual([]);
    expect(s.activeProjectId).toBeNull();
    expect(s.projectStates["p1"]).toBeUndefined();
    expect(s.messages["p1"]).toBeUndefined();
    expect(s.permissions["p1"]).toBeUndefined();
    expect(s.phases["p1"]).toBeUndefined();
    expect(s.revisions["p1"]).toBeUndefined();
    expect(s.shownTurns["p1"]).toBeUndefined();
  });

  it("forgetProject é idempotente", () => {
    seedProject("p1");
    appStore.forgetProject("p1");
    expect(() => appStore.forgetProject("p1")).not.toThrow();
    expect(appStore.getState().config?.projects.map((p) => p.id)).toEqual(["p2"]);
  });

  it("evento project_removed no SSE é aplicado ao store e não recria estado", () => {
    seedProject("p1");

    appStore.applyEvent({ event_type: "project_removed", project_id: "p1", payload: { project_id: "p1" } });
    appStore.applyEvent({ event_type: "agent_message", project_id: "p1", payload: { agent: "developer", text: "tarde" } });
    appStore.applyEvent({ event_type: "project_status", project_id: "p1", payload: { status: "executing", spec_id: "s" } });

    const s = appStore.getState();
    expect(s.config?.projects.map((p) => p.id)).toEqual(["p2"]);
    expect(s.messages["p1"]).toBeUndefined();
    expect(s.phases["p1"]).toBeUndefined();
  });

  it("depois de removido, respostas do Arquiteto e permissões do id são descartadas", () => {
    seedProject("p1");
    appStore.forgetProject("p1");

    expect(appStore.showArchitectReply("p1", "resposta", "t9")).toBe(false);
    appStore.hydrateHistory("p1", [{ role: "architect", text: "histórico", ts: 1, turn_id: "t2" }]);
    appStore.hydratePermissions("p1", [{ request_id: "r2", task_id: null, agent: "developer", command: "ls" }], 0);
    appStore.clearPermission("p1", "r2");
    appStore.confirmUserMessage("p1", 1, "t3");

    const s = appStore.getState();
    expect(s.messages["p1"]).toBeUndefined();
    expect(s.permissions["p1"]).toBeUndefined();
    expect(s.shownTurns["p1"]).toBeUndefined();
  });

  it("readmitProject libera o id quando o mesmo caminho é cadastrado de novo", () => {
    seedProject("p1");
    appStore.forgetProject("p1");
    appStore.readmitProject("p1");

    appStore.applyEvent({ event_type: "agent_message", project_id: "p1", payload: { agent: "developer", text: "volta" } });

    expect(appStore.getState().messages["p1"]?.map((m) => m.text)).toEqual(["volta"]);
  });
});

describe("resíduos: GET atrasado de projeto removido", () => {
  it("GET de estado que chega depois da remoção não recria projectStates", async () => {
    seedProject("p1");
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    installFetch(async () => {
      await gate;
      return { body: makeProjectState({ id: "p1" }) };
    });

    const pending = refreshProjectState("p1");
    appStore.forgetProject("p1");
    release();
    await pending;

    expect(appStore.getState().projectStates["p1"]).toBeUndefined();
    expect(appStore.getState().permissions["p1"]).toBeUndefined();
  });

  it("GET de histórico que chega depois da remoção não recria mensagens", async () => {
    seedProject("p1");
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    installFetch(async () => {
      await gate;
      return { body: { turns: [{ role: "user", text: "antigo", ts: 1, turn_id: "t1" }] } };
    });

    const pending = loadChatHistory("p1");
    appStore.forgetProject("p1");
    release();
    await pending;

    expect(appStore.getState().messages["p1"]).toBeUndefined();
    expect(appStore.getState().shownTurns["p1"]).toBeUndefined();
  });

  it("GET de estado após a remoção não faz requisição que recrie nada e não lança", async () => {
    seedProject("p1");
    appStore.forgetProject("p1");
    installFetch(() => ({ body: makeProjectState({ id: "p1" }) }));

    await expect(refreshProjectState("p1")).resolves.toBeNull();
    expect(appStore.getState().projectStates["p1"]).toBeUndefined();
  });
});
