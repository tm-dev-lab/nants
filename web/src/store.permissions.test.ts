import { beforeEach, describe, expect, it } from "vitest";
import { appStore, arrivalClockNow, MAX_RESOLVED_PERMISSIONS } from "./store";
import type { PendingPermission, ResolvedPermission } from "./store";
import type { ApiEvent, PendingPermissionView } from "./api/types";
import { resetStore } from "./test-utils";

const PID = "p1";

function requested(requestId: string, extra: Record<string, unknown> = {}): ApiEvent {
  return {
    event_type: "permission_requested",
    project_id: PID,
    payload: {
      request_id: requestId,
      project_id: PID,
      task_id: "T01",
      agent: "developer",
      command: "node --test",
      always_allowed: false,
      always_denied_reason: "este comando executa código arbitrário",
      ...extra,
    },
  } as unknown as ApiEvent;
}

function resolved(requestId: string, applied: string): ApiEvent {
  return {
    event_type: "permission_resolved",
    project_id: PID,
    payload: { request_id: requestId, project_id: PID, applied },
  } as unknown as ApiEvent;
}

function pendingIds(): string[] {
  return (appStore.getState().permissions[PID] ?? []).map((p) => p.requestId);
}

function resolvedList(): ResolvedPermission[] {
  return appStore.getState().resolvedPermissions[PID] ?? [];
}

function serverPending(requestId: string): PendingPermissionView {
  return { request_id: requestId, agent: "developer", command: "node --test", task_id: "T01" };
}

beforeEach(() => resetStore());

describe("permission_resolved: pedido vai para a lista de resolvidos com o escopo aplicado", () => {
  it("move o pendente para resolvidos com applied do evento, sem duplicar", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent(resolved("r1", "run"));

    expect(pendingIds()).toEqual([]);
    expect(resolvedList().map((r) => [r.requestId, r.applied])).toEqual([["r1", "run"]]);
    expect(typeof resolvedList()[0]?.resolvedAt).toBe("number");
  });

  it("evento repetido ou de pedido desconhecido não cria entrada", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent(resolved("r1", "once"));
    appStore.applyEvent(resolved("r1", "once"));
    appStore.applyEvent(resolved("r9", "once"));

    expect(resolvedList().map((r) => r.requestId)).toEqual(["r1"]);
  });

  it("um pedido já resolvido que volta por SSE (replay) não reaparece como pendente", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent(resolved("r1", "deny"));
    appStore.applyEvent(requested("r1"));

    expect(pendingIds()).toEqual([]);
    expect(resolvedList().map((r) => r.requestId)).toEqual(["r1"]);
  });

  it("settlePermission após o POST é idempotente com o evento que chegou antes", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent(resolved("r1", "once"));
    appStore.settlePermission(PID, "r1", "once");

    expect(resolvedList().map((r) => [r.requestId, r.applied])).toEqual([["r1", "once"]]);
  });

  it("settlePermission guarda o escolhido quando difere do aplicado (Sempre virou run)", () => {
    appStore.applyEvent(requested("r1", { always_allowed: true, always_denied_reason: null }));
    appStore.settlePermission(PID, "r1", "run", "always");

    const [entry] = resolvedList();
    expect(entry?.applied).toBe("run");
    expect(entry?.requested).toBe("always");
    expect(pendingIds()).toEqual([]);
  });

  it("settlePermission com applied null (404: já respondido em outra janela) mantém o pedido visível como resolvido", () => {
    appStore.applyEvent(requested("r1"));
    appStore.settlePermission(PID, "r1", null);

    expect(pendingIds()).toEqual([]);
    expect(resolvedList().map((r) => [r.requestId, r.applied])).toEqual([["r1", null]]);
  });

  it("resolvidos têm teto por projeto (os mais antigos saem)", () => {
    const total = MAX_RESOLVED_PERMISSIONS + 5;
    for (let i = 0; i < total; i += 1) {
      appStore.applyEvent(requested(`r${i}`));
      appStore.applyEvent(resolved(`r${i}`, "once"));
    }

    const ids = resolvedList().map((r) => r.requestId);
    expect(ids.length).toBe(MAX_RESOLVED_PERMISSIONS);
    expect(ids[0]).toBe("r5");
    expect(ids[ids.length - 1]).toBe(`r${total - 1}`);
  });
});

describe("pendente e reidratação", () => {
  it("pedido novo recebe o instante de chegada (at)", () => {
    const before = Date.now();
    appStore.applyEvent(requested("r1"));

    const at = appStore.getState().permissions[PID]?.[0]?.at ?? 0;
    expect(at).toBeGreaterThanOrEqual(before);
  });

  it("reidratação não duplica um pendente já exibido (merge por request_id)", () => {
    appStore.applyEvent(requested("r1"));
    appStore.hydratePermissions(PID, [serverPending("r1")], 0);
    appStore.hydratePermissions(PID, [serverPending("r1")], 0);

    expect(pendingIds()).toEqual(["r1"]);
  });

  it("reload: pendente do servidor reaparece com as opções (fila vazia antes)", () => {
    appStore.hydratePermissions(PID, [serverPending("r7")], 0);

    const [entry] = appStore.getState().permissions[PID] ?? [];
    expect(entry?.requestId).toBe("r7");
    expect(entry?.taskId).toBe("T01");
    expect(typeof entry?.at).toBe("number");
  });

  it("reidratação não traz de volta um pedido já resolvido aqui (snapshot antigo do GET)", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent(resolved("r1", "run"));
    appStore.hydratePermissions(PID, [serverPending("r1")], 0);

    expect(pendingIds()).toEqual([]);
    expect(resolvedList().map((r) => r.requestId)).toEqual(["r1"]);
  });

  it("pendente que o servidor não tem mais (e não chegou depois do GET) vira resolvido sem decisão", () => {
    appStore.applyEvent(requested("r1"));
    // GET iniciado depois da chegada: o pedido já estava aqui antes dele.
    appStore.hydratePermissions(PID, [], arrivalClockNow());

    expect(pendingIds()).toEqual([]);
    expect(resolvedList().map((r) => [r.requestId, r.applied])).toEqual([["r1", null]]);
  });

  it("encerramento do projeto (status fora de executing) registra os pendentes como resolvidos sem decisão", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent({
      event_type: "project_status",
      project_id: PID,
      payload: { status: "idle" },
    } as unknown as ApiEvent);

    expect(pendingIds()).toEqual([]);
    expect(resolvedList().map((r) => [r.requestId, r.applied])).toEqual([["r1", null]]);
  });
});

describe("tipos", () => {
  it("resolvido é um pendente com applied e instante de resolução", () => {
    appStore.applyEvent(requested("r1"));
    appStore.applyEvent(resolved("r1", "deny"));
    const entry: PendingPermission | undefined = resolvedList()[0];
    expect(entry?.command).toBe("node --test");
  });
});
