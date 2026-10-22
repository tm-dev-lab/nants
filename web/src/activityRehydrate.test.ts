import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { refreshActivities } from "./activityRehydrate";
import { appStore, mergeServerActivities } from "./store";
import type { ActivityEntry } from "./store";
import type { ActivitySnapshot } from "./api/types";

const snap = (o: Partial<ActivitySnapshot> = {}): ActivitySnapshot => ({
  agent: "developer", task_id: "T01", spec_id: "s1", activity_id: "a1",
  reasoning: "pensando", text: "", tool: "lendo x", age_ms: 2_000, ...o,
});

describe("reidratação de atividade", () => {
  beforeEach(() => { vi.useFakeTimers(); vi.setSystemTime(1_000_000); });
  afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

  it("mescla com idade: lastAt = agora - age_ms", () => {
    const out = mergeServerActivities([], [snap()], 1_000_000);
    expect(out).toHaveLength(1);
    expect(out[0]?.lastAt).toBe(998_000);
    expect(out[0]?.reasoning).toBe("pensando");
  });

  it("entrada local mais nova não é sobrescrita", () => {
    const local: ActivityEntry = { key: JSON.stringify(["developer", "T01"]), agent: "developer", taskId: "T01", activityId: "a1", reasoning: "novo", text: "", tool: "", startedAt: 0, lastAt: 999_500 };
    const out = mergeServerActivities([local], [snap()], 1_000_000);
    expect(out[0]?.reasoning).toBe("novo");
  });

  it("entrada local mais antiga é substituída; local ausente no servidor permanece", () => {
    const old: ActivityEntry = { key: JSON.stringify(["developer", "T01"]), agent: "developer", taskId: "T01", activityId: "a1", reasoning: "velho", text: "", tool: "", startedAt: 0, lastAt: 990_000 };
    const other: ActivityEntry = { ...old, key: JSON.stringify(["reviewer", "T01"]), agent: "reviewer", endedAt: 999_000 };
    const out = mergeServerActivities([old, other], [snap()], 1_000_000);
    expect(out.find((a) => a.agent === "developer")?.reasoning).toBe("pensando");
    expect(out.find((a) => a.agent === "reviewer")).toBeDefined();
  });

  it("item vencido (idle) é ignorado", () => {
    expect(mergeServerActivities([], [snap({ age_ms: 130_000 })], 1_000_000)).toHaveLength(0);
  });

  it("resposta atrasada (mais antiga) é ignorada; falha de rede não quebra", async () => {
    const resolvers: Array<(r: Response) => void> = [];
    vi.stubGlobal("fetch", vi.fn(() => new Promise<Response>((res) => resolvers.push(res))));
    const body = (id: string) => new Response(JSON.stringify({ activities: [snap({ activity_id: id })] }), { status: 200, headers: { "content-type": "application/json" } });
    const p1 = refreshActivities("pz");
    const p2 = refreshActivities("pz");
    resolvers[1]?.(body("novo"));
    await p2;
    resolvers[0]?.(body("antigo"));
    await p1;
    expect(appStore.getState().activities["pz"]?.[0]?.activityId).toBe("novo");

    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new Error("offline"))));
    await expect(refreshActivities("pw")).resolves.toBeUndefined();
    expect(appStore.getState().activities["pw"]).toBeUndefined();
  });
});
