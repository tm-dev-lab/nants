import { describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "./api/types";
import { liveNow } from "./liveClock";
import { createAppStore } from "./store";

const P = "p-seq";

describe("ChatMessage.seq (chegadas ao vivo)", () => {
  it("mensagens ao vivo recebem seq crescente; o histórico não recebe", () => {
    const store = createAppStore();
    store.appendMessage(P, { role: "user", text: "a" });
    store.showArchitectReply(P, "b", "t1");
    store.hydrateHistory(P, [{ role: "user", text: "h", ts: 1, turn_id: "t0" }]);
    const list = store.getState().messages[P] ?? [];
    const live = list.filter((m) => m.origin === "live").map((m) => m.seq ?? 0);
    expect(live[0]).toBeGreaterThan(0);
    expect(live[1]).toBeGreaterThan(live[0] ?? 0);
    expect(list.filter((m) => m.origin === "history").every((m) => m.seq === undefined)).toBe(true);
  });

  it("live mantida na hidratação conserva o seq; a próxima live continua o contador", () => {
    const store = createAppStore();
    store.appendMessage(P, { role: "user", text: "pendente" });
    const before = store.getState().messages[P]?.[0]?.seq ?? 0;
    store.hydrateHistory(P, [{ role: "architect", text: "x", ts: 1, turn_id: "old" }]);
    const kept = store.getState().messages[P]?.find((m) => m.text === "pendente");
    expect(kept?.seq).toBe(before);
    store.appendMessage(P, { role: "user", text: "nova" });
    const next = store.getState().messages[P]?.find((m) => m.text === "nova")?.seq ?? 0;
    expect(next).toBeGreaterThan(before);
  });

  it("a hora das mensagens ao vivo não recua quando o relógio de parede volta (114/T01)", () => {
    const store = createAppStore();
    // Acima da hora real e de tudo o que o relógio da linha do tempo (global) já deu: vale em
    // qualquer data e em qualquer ordem dos testes.
    const base = liveNow() + 60_000;
    let wall = base;
    vi.spyOn(Date, "now").mockImplementation(() => wall);
    store.appendMessage(P, { role: "architect", text: "pergunta" });
    // O WSL ajusta a hora para trás alguns segundos (journald: "Time jumped backwards").
    wall -= 3_000;
    store.appendMessage(P, { role: "user", text: "resposta" });
    store.showArchitectReply(P, "ok", "t1");
    store.beginArchitectTurn(P, { turnId: "t2", startedAt: wall, userText: "outra aba" });
    store.applyEvent({ event_type: "agent_message", project_id: P, payload: { agent: "developer", text: "agente", spec_id: "s1" } });
    const at = (store.getState().messages[P] ?? []).map((m) => m.at);
    expect(at).toEqual([base, base, base, base, base]);
    // Com o relógio de novo à frente, a hora volta a ser a do relógio.
    wall += 10_000;
    store.appendMessage(P, { role: "user", text: "depois" });
    expect(store.getState().messages[P]?.at(-1)?.at).toBe(wall);
  });

  it("pedidos de permissão e resoluções também não recuam na linha do tempo (114/T01)", () => {
    const store = createAppStore();
    const base = liveNow() + 60_000;
    let wall = base;
    vi.spyOn(Date, "now").mockImplementation(() => wall);
    const request = (id: string): ApiEvent =>
      ({
        event_type: "permission_requested",
        project_id: P,
        payload: { request_id: id, project_id: P, task_id: "T01", agent: "developer", command: "npm test", always_allowed: true },
      }) as unknown as ApiEvent;
    store.appendMessage(P, { role: "architect", text: "antes" });
    wall -= 3_000;
    store.applyEvent(request("r1"));
    store.applyEvent(request("r2"));
    store.applyEvent(request("r3"));
    store.applyEvent({ event_type: "permission_resolved", project_id: P, payload: { request_id: "r1", project_id: P, applied: "once" } } as unknown as ApiEvent);
    store.settlePermission(P, "r2", "deny", "deny");
    store.hydratePermissions(P, [{ request_id: "r4", agent: "developer", command: "ls", task_id: "T01" }], 0);
    store.applyEvent({ event_type: "project_status", project_id: P, payload: { status: "idle" } } as unknown as ApiEvent);
    const pendingAt = (store.getState().permissions[P] ?? []).map((p) => p.at);
    const resolved = store.getState().resolvedPermissions[P] ?? [];
    expect(pendingAt.every((at) => at === base)).toBe(true);
    expect(resolved.map((r) => r.requestId).sort()).toEqual(["r1", "r2", "r3", "r4"]);
    expect(resolved.every((r) => r.at === base && r.resolvedAt === base)).toBe(true);
  });
});
