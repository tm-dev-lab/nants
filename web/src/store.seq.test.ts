import { describe, expect, it } from "vitest";
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
});
