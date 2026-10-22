import { describe, expect, it } from "vitest";
import type { ApiEvent } from "./api/types";
import { createAppStore, MAX_MESSAGES_PER_PROJECT, MAX_PENDING_PERMISSIONS } from "./store";

const PID = "proj-1";

function agentMessage(text: string, agent = "architect"): ApiEvent {
  return { event_type: "agent_message", project_id: PID, payload: { agent, text } };
}

function permission(requestId: string, command = "ls"): ApiEvent {
  return {
    event_type: "permission_requested",
    project_id: PID,
    payload: { request_id: requestId, project_id: PID, task_id: "T01", agent: "developer", command },
  };
}

describe("store: mensagens", () => {
  it("mantém no máximo 500 mensagens por projeto, descartando as mais antigas", () => {
    const store = createAppStore();
    for (let i = 0; i < MAX_MESSAGES_PER_PROJECT + 10; i += 1) {
      store.appendMessage(PID, { role: "user", text: `m${i}` });
    }
    const list = store.getState().messages[PID] ?? [];
    expect(MAX_MESSAGES_PER_PROJECT).toBe(500);
    expect(list).toHaveLength(500);
    expect(list[0]?.text).toBe("m10");
    expect(list.at(-1)?.text).toBe(`m${MAX_MESSAGES_PER_PROJECT + 9}`);
  });

  it("aplica o cap também a mensagens vindas de eventos", () => {
    const store = createAppStore();
    for (let i = 0; i < 505; i += 1) store.applyEvent(agentMessage(`e${i}`));
    expect(store.getState().messages[PID]).toHaveLength(500);
  });

  it("guarda texto puro sem interpretar HTML", () => {
    const store = createAppStore();
    store.applyEvent(agentMessage("<img src=x onerror=alert(1)>"));
    expect(store.getState().messages[PID]?.[0]?.text).toBe("<img src=x onerror=alert(1)>");
  });

  it("mensagens são separadas por projeto", () => {
    const store = createAppStore();
    store.appendMessage("a", { role: "user", text: "x" });
    store.appendMessage("b", { role: "user", text: "y" });
    expect(store.getState().messages.a).toHaveLength(1);
    expect(store.getState().messages.b).toHaveLength(1);
  });
});

describe("store: status derivado dos eventos", () => {
  it("começa como idle e segue planejamento, aprovação, execução e integração", () => {
    const store = createAppStore();
    const phase = () => store.getState().phases[PID] ?? "idle";
    expect(phase()).toBe("idle");

    store.applyEvent({ event_type: "sdd_draft_updated", project_id: PID, payload: { project_id: PID } });
    expect(phase()).toBe("planning");

    store.applyEvent({
      event_type: "permission_requested",
      project_id: PID,
      payload: {
        request_id: "r1",
        project_id: PID,
        task_id: "T01",
        agent: "developer",
        command: "cargo test",
      },
    });
    expect(phase()).toBe("waiting_approval");
    expect(store.getState().permissions[PID]).toEqual([
      { requestId: "r1", projectId: PID, taskId: "T01", agent: "developer", command: "cargo test", at: expect.any(Number) },
    ]);

    store.applyEvent({
      event_type: "task_updated",
      project_id: PID,
      payload: { spec_id: "005", task_id: "T01", status: "IN_PROGRESS" },
    });
    expect(phase()).toBe("executing");

    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "integrated", spec_id: "005" },
    });
    expect(phase()).toBe("integrated");
  });

  it("project_status blocked e idle definem a fase", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "blocked", task_id: "T02", reason: "task_blocked" },
    });
    expect(store.getState().phases[PID]).toBe("blocked");
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "idle", reason: "shutdown" },
    });
    expect(store.getState().phases[PID]).toBe("idle");
  });

  it("rascunho não sobrescreve execução em andamento", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "executing", spec_id: "005" },
    });
    store.applyEvent({ event_type: "sdd_draft_updated", project_id: PID, payload: { project_id: PID } });
    expect(store.getState().phases[PID]).toBe("executing");
  });

  it("eventos globais não alteram o estado de projeto", () => {
    const store = createAppStore();
    store.applyEvent({ event_type: "sdd_draft_updated", project_id: null, payload: {} } as unknown as ApiEvent);
    expect(store.getState().phases).toEqual({});
  });

  it("blocked não é sobrescrito por sdd_draft_updated nem por task_updated não-BLOCKED", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "blocked", task_id: "T02", reason: "task_blocked" },
    });
    store.applyEvent({ event_type: "sdd_draft_updated", project_id: PID, payload: { project_id: PID } });
    expect(store.getState().phases[PID]).toBe("blocked");
    store.applyEvent({
      event_type: "task_updated",
      project_id: PID,
      payload: { spec_id: "005", task_id: "T02", status: "IN_PROGRESS" },
    });
    expect(store.getState().phases[PID]).toBe("blocked");
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "executing", spec_id: "005" },
    });
    expect(store.getState().phases[PID]).toBe("executing");
  });

  it("task BLOCKED seguida de RETRY e task em andamento volta a executing sem project_status", () => {
    const store = createAppStore();
    store.applyEvent({ event_type: "task_updated", project_id: PID, payload: { spec_id: "005", task_id: "T01", status: "BLOCKED" } });
    expect(store.getState().phases[PID]).toBe("blocked");
    store.applyEvent({ event_type: "task_updated", project_id: PID, payload: { spec_id: "005", task_id: "T01", status: "IN_PROGRESS" } });
    expect(store.getState().phases[PID]).toBe("executing");
  });

  it("integrated não é sobrescrito por sdd_draft_updated", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "integrated", spec_id: "005" },
    });
    store.applyEvent({ event_type: "sdd_draft_updated", project_id: PID, payload: { project_id: PID } });
    expect(store.getState().phases[PID]).toBe("integrated");
  });

  it("limita permissões pendentes a 64, descartando as mais antigas", () => {
    const store = createAppStore();
    for (let i = 0; i < MAX_PENDING_PERMISSIONS + 5; i += 1) {
      store.applyEvent(permission(`r${i}`));
    }
    const list = store.getState().permissions[PID] ?? [];
    expect(MAX_PENDING_PERMISSIONS).toBe(64);
    expect(list).toHaveLength(64);
    expect(list[0]?.requestId).toBe("r5");
    expect(list.at(-1)?.requestId).toBe(`r${MAX_PENDING_PERMISSIONS + 4}`);
  });

  it("permission_requested com task_id null entra em permissions e vira waiting_approval", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "permission_requested",
      project_id: PID,
      payload: { request_id: "r9", project_id: PID, task_id: null, agent: "developer", command: "cargo test" },
    });
    expect(store.getState().phases[PID]).toBe("waiting_approval");
    expect(store.getState().permissions[PID]).toEqual([
      { requestId: "r9", projectId: PID, taskId: null, agent: "developer", command: "cargo test", at: expect.any(Number) },
    ]);
  });

  it("deduplica permissões por request_id", () => {
    const store = createAppStore();
    store.applyEvent(permission("r1", "ls"));
    store.applyEvent(permission("r2", "pwd"));
    store.applyEvent(permission("r1", "ls -la"));
    const list = store.getState().permissions[PID] ?? [];
    expect(list.map((p) => p.requestId)).toEqual(["r1", "r2"]);
    expect(list[0]?.command).toBe("ls -la");
  });

  it("limpa permissões pendentes em idle, integrated e blocked, mas não em executing", () => {
    for (const status of ["idle", "integrated", "blocked"] as const) {
      const store = createAppStore();
      store.applyEvent(permission("r1"));
      const payload =
        status === "idle"
          ? { status, reason: "shutdown" }
          : status === "integrated"
            ? { status, spec_id: "005" }
            : { status, task_id: "T01", reason: "task_blocked" };
      store.applyEvent({ event_type: "project_status", project_id: PID, payload });
      expect(store.getState().permissions[PID], status).toEqual([]);
    }
    const store = createAppStore();
    store.applyEvent(permission("r1"));
    store.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "executing", spec_id: "005" },
    });
    expect(store.getState().permissions[PID]).toHaveLength(1);
  });

  it("clearPermission remove apenas o pedido informado", () => {
    const store = createAppStore();
    const perm = (id: string): ApiEvent => ({
      event_type: "permission_requested",
      project_id: PID,
      payload: { request_id: id, project_id: PID, task_id: "T01", agent: "developer", command: "ls" },
    });
    store.applyEvent(perm("r1"));
    store.applyEvent(perm("r2"));
    store.clearPermission(PID, "r1");
    expect(store.getState().permissions[PID]?.map((p) => p.requestId)).toEqual(["r2"]);
  });

  it("notifica assinantes a cada mudança", () => {
    const store = createAppStore();
    let calls = 0;
    const off = store.subscribe(() => {
      calls += 1;
    });
    store.setState({ activeProjectId: PID });
    off();
    store.setState({ activeProjectId: null });
    expect(calls).toBe(1);
  });
});

describe("store: permission_resolved", () => {
  const resolved = (requestId: string): ApiEvent => ({
    event_type: "permission_resolved",
    project_id: PID,
    payload: { request_id: requestId, project_id: PID, applied: "once" },
  });

  it("remove só o pedido resolvido e mantém a fase enquanto sobra pedido", () => {
    const store = createAppStore();
    store.applyEvent(permission("r1"));
    store.applyEvent(permission("r2"));

    store.applyEvent(resolved("r1"));

    expect(store.getState().permissions[PID]?.map((p) => p.requestId)).toEqual(["r2"]);
    expect(store.getState().phases[PID]).toBe("waiting_approval");
  });

  it("sem pedidos restantes, sai de aguardando aprovação", () => {
    const store = createAppStore();
    store.applyEvent(permission("r1"));

    store.applyEvent(resolved("r1"));

    expect(store.getState().permissions[PID]).toEqual([]);
    expect(store.getState().phases[PID]).toBeUndefined();
  });

  it("pedido desconhecido não altera o estado", () => {
    const store = createAppStore();
    store.applyEvent(permission("r1"));
    const before = store.getState();

    store.applyEvent(resolved("outro"));

    expect(store.getState().permissions).toBe(before.permissions);
  });

  it("guarda always_allowed e always_denied_reason do pedido", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "permission_requested",
      project_id: PID,
      payload: {
        request_id: "r1",
        project_id: PID,
        task_id: null,
        agent: "developer",
        command: "node --test",
        always_allowed: false,
        always_denied_reason: "este comando executa código arbitrário",
      },
    });

    expect(store.getState().permissions[PID]?.[0]).toMatchObject({
      alwaysAllowed: false,
      alwaysDeniedReason: "este comando executa código arbitrário",
    });
  });
});

describe("store: fim do turno do Arquiteto (spec 094, achado 8)", () => {
  it("chat_turn_ended encerra o turno e sobe `revisions` (recarrega o estado do projeto)", () => {
    const store = createAppStore();
    store.applyEvent({ event_type: "chat_turn_started", project_id: PID, payload: { project_id: PID, turn_id: "t094", user_text: "plano", started_at: 1 } } as ApiEvent);
    expect(store.getState().architectTurns[PID]?.turnId).toBe("t094");
    const before = store.getState().revisions[PID] ?? 0;
    store.applyEvent({ event_type: "chat_turn_ended", project_id: PID, payload: { project_id: PID, turn_id: "t094", ok: true } } as ApiEvent);
    expect(store.getState().architectTurns[PID]).toBeUndefined();
    expect(store.getState().revisions[PID]).toBe(before + 1);
  });
});
