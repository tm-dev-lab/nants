import { describe, expect, it } from "vitest";
import { createAppStore, MAX_MESSAGES_PER_PROJECT } from "./store";
import type { ChatMessage } from "./store";
import type { ApiEvent, ChatHistoryTurn, DialogMessage } from "./api/types";
import { countNewer } from "./apps/conversation/AutoScrollBox";
import { providerFailureMessage } from "./copy";

const PID = "p1";
const BASE = 1_700_000_000_000_000;

function row(i: number, overrides: Partial<DialogMessage> = {}): DialogMessage {
  return {
    seq: BASE + i,
    ts: 1_700_000_000 + i,
    agent: "developer",
    task_id: "T02",
    attempt: 1,
    text: `r${i}`,
    ...overrides,
  };
}

function live(text: string, seq?: number, agent = "developer", taskId?: string): ApiEvent {
  return {
    event_type: "agent_message",
    project_id: PID,
    payload: {
      agent,
      text,
      // O servidor sempre informa a spec do destino (ver `dialog::deliver`).
      spec_id: "s1",
      ...(seq === undefined ? {} : { seq }),
      ...(taskId === undefined ? {} : { task_id: taskId }),
    },
  };
}

function agentTexts(store: ReturnType<typeof createAppStore>): string[] {
  return (store.getState().messages[PID] ?? []).filter((m) => m.role === "agent").map((m) => m.text);
}

function agentRows(store: ReturnType<typeof createAppStore>): ChatMessage[] {
  return (store.getState().messages[PID] ?? []).filter((m) => m.role === "agent");
}

describe("hydrateDialog: linhas persistidas", () => {
  it("vira role agent, origem history, com dialogSeq, taskId, attempt e texto traduzido", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [
      row(1, { agent: "system", task_id: "T01", attempt: 2, text: "provider_http_400" }),
    ]);

    const [msg] = agentRows(store);
    expect(msg).toMatchObject({
      role: "agent",
      agent: "system",
      origin: "history",
      dialogSeq: BASE + 1,
      taskId: "T01",
      attempt: 2,
      at: (1_700_000_000 + 1) * 1000,
    });
    expect(msg?.text).toBe(providerFailureMessage("provider_http_400", null));
    expect(msg?.seq).toBeUndefined();
  });

  it("mesmo texto ao vivo e hidratado: a tradução é a mesma", () => {
    const store = createAppStore();
    store.applyEvent(live("provider_timeout", BASE + 1, "system"));
    store.hydrateDialog(PID, "s1", [row(1, { agent: "system", text: "provider_timeout" })]);

    expect(agentTexts(store)).toEqual([providerFailureMessage("provider_timeout", null)]);
  });

  it("falha do provedor com detalhe mostra o motivo; sem detalhe, fica como antes (spec 088)", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [
      row(1, { agent: "system", text: "provider_bad_response (incompleta: content_filter)" }),
      row(2, { agent: "system", text: "provider_bad_response" }),
      row(3, { agent: "system", text: "backend_error (vazia)" }),
      row(4, { agent: "system", text: `provider_bad_response (${"x".repeat(81)})` }),
      row(5, { agent: "developer", text: "provider_bad_response (vazia)" }),
    ]);

    expect(agentTexts(store)).toEqual([
      "O provedor devolveu uma resposta inesperada (incompleta: content_filter).",
      "O provedor devolveu uma resposta inesperada.",
      "backend_error (vazia)",
      "O provedor devolveu uma resposta inesperada.",
      "provider_bad_response (vazia)",
    ]);
  });

  it("falha por limite do provedor ganha a dica; as demais não (spec 089)", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [
      row(1, { agent: "system", text: "provider_bad_response (provedor no limite: no_capacity)" }),
      row(2, { agent: "system", text: "provider_http_429" }),
      row(3, { agent: "system", text: "provider_bad_response (incompleta: content_filter)" }),
    ]);
    store.applyEvent(live("provider_bad_response (provedor no limite)", BASE + 4, "system"));

    expect(agentTexts(store)).toEqual([
      "O provedor devolveu uma resposta inesperada (provedor no limite: no_capacity). O provedor está limitando as requisições. O Nants já reduz o ritmo sozinho; continue em alguns minutos.",
      "O provedor limitou as requisições (HTTP 429). O Nants já reduz o ritmo sozinho; continue em alguns minutos.",
      "O provedor devolveu uma resposta inesperada (incompleta: content_filter).",
      "O provedor devolveu uma resposta inesperada (provedor no limite). O provedor está limitando as requisições. O Nants já reduz o ritmo sozinho; continue em alguns minutos.",
    ]);
  });

  it("falha do provedor com detalhe ao vivo usa a mesma tradução", () => {
    const store = createAppStore();
    store.applyEvent(live("provider_bad_response (vazia)", BASE + 1, "system"));

    expect(agentTexts(store)).toEqual(["O provedor devolveu uma resposta inesperada (vazia)."]);
  });

  it("preserva quebras de linha e blocos de código (texto puro)", () => {
    const store = createAppStore();
    const text = "primeira linha\n\n```sh\nmake test\n```";
    store.hydrateDialog(PID, "s1", [row(1, { text })]);

    expect(agentTexts(store)).toEqual([text]);
  });

  it("repetir a hidratação com o mesmo conjunto não cresce a lista", () => {
    const store = createAppStore();
    const rows = [row(1), row(2), row(3)];
    for (let i = 0; i < 5; i += 1) store.hydrateDialog(PID, "s1", rows);

    expect(agentTexts(store)).toEqual(["r1", "r2", "r3"]);
  });

  it("linha persistida nova substitui o conjunto anterior (fonte da verdade)", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(1), row(2)]);
    store.hydrateDialog(PID, "s1", [row(2), row(3)]);

    expect(agentTexts(store)).toEqual(["r2", "r3"]);
  });
});

describe("hydrateDialog: ao vivo x persistido", () => {
  it("descarta ao vivo cujo dialogSeq já está no servidor; mantém seq nulo e posterior ao last_seq", () => {
    const store = createAppStore();
    store.applyEvent(live("duplicada-ao-vivo", BASE + 2));
    store.applyEvent(live("ainda-nao-gravada")); // seq nulo
    store.applyEvent(live("posterior", BASE + 9)); // posterior ao último lido (3)

    store.hydrateDialog(PID, "s1", [row(1), row(2), row(3)]);

    expect(agentTexts(store)).toEqual(["r1", "r2", "r3", "posterior", "ainda-nao-gravada"]);
  });

  it("ao vivo com dialogSeq ausente do servidor e anterior ao último lido é descartado (rotação)", () => {
    const store = createAppStore();
    store.applyEvent(live("girada", BASE + 1));

    store.hydrateDialog(PID, "s1", [row(5), row(6)]);

    expect(agentTexts(store)).toEqual(["r5", "r6"]);
  });

  it("evento com seq já presente no histórico não duplica", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(1)]);
    store.applyEvent(live("r1", BASE + 1));

    expect(agentTexts(store)).toEqual(["r1"]);
  });

  it("o mesmo evento com seq chegando duas vezes aparece uma vez", () => {
    const store = createAppStore();
    store.applyEvent(live("uma", BASE + 4));
    store.applyEvent(live("uma", BASE + 4));

    expect(agentTexts(store)).toEqual(["uma"]);
  });

  it("evento ao vivo guarda dialogSeq, taskId e attempt do payload", () => {
    const store = createAppStore();
    store.applyEvent({
      event_type: "agent_message",
      project_id: PID,
      payload: { agent: "reviewer", text: "ok", seq: BASE + 7, task_id: "T03", attempt: 2 },
    });

    expect(agentRows(store)[0]).toMatchObject({ dialogSeq: BASE + 7, taskId: "T03", attempt: 2 });
  });

  it("mescla (mergeDialog) só adiciona seq novo e mantém a ordem por dialogSeq", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(1), row(2), row(3)]);
    store.mergeDialog(PID, "s1", [row(3), row(4), row(5)]);
    store.mergeDialog(PID, "s1", [row(4)]);

    expect(agentTexts(store)).toEqual(["r1", "r2", "r3", "r4", "r5"]);
  });
});

describe("hydrateDialog x chat do Arquiteto (sem interferência)", () => {
  const turns: ChatHistoryTurn[] = [
    { role: "user", text: "pergunta", ts: 100, turn_id: "0123456789abcdef0123456789abcdef" },
    { role: "architect", text: "resposta", ts: 101, turn_id: "fedcba9876543210fedcba9876543210" },
  ];

  it("hydrateHistory não apaga o diálogo hidratado", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(1), row(2)]);
    store.hydrateHistory(PID, turns);
    store.hydrateHistory(PID, turns);

    expect(agentTexts(store)).toEqual(["r1", "r2"]);
    expect(store.getState().messages[PID]?.filter((m) => m.role !== "agent").map((m) => m.text)).toEqual([
      "pergunta",
      "resposta",
    ]);
  });

  it("hydrateDialog não apaga o histórico do Arquiteto", () => {
    const store = createAppStore();
    store.hydrateHistory(PID, turns);
    store.hydrateDialog(PID, "s1", [row(1)]);
    store.hydrateDialog(PID, "s1", []);

    expect(store.getState().messages[PID]?.filter((m) => m.role !== "agent").map((m) => m.text)).toEqual([
      "pergunta",
      "resposta",
    ]);
  });

  it("teto por família: 600 linhas do diálogo mantêm as 500 mais recentes e não evictam o Arquiteto", () => {
    const store = createAppStore();
    store.hydrateHistory(PID, turns);
    store.hydrateDialog(PID, "s1", Array.from({ length: 600 }, (_, k) => row(k + 1)));

    const agents = agentTexts(store);
    expect(agents).toHaveLength(MAX_MESSAGES_PER_PROJECT);
    expect(agents[0]).toBe("r101");
    expect(agents.at(-1)).toBe("r600");
    expect(store.getState().messages[PID]?.filter((m) => m.role !== "agent")).toHaveLength(2);
  });
});

describe("linhas de histórico e o contador de novas", () => {
  it("linhas hidratadas não têm seq local: não contam como novas no AutoScrollBox", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(1), row(2)]);
    const items = agentRows(store);

    expect(items.every((m) => m.seq === undefined)).toBe(true);
    expect(countNewer(items, 0)).toBe(0);
  });

  it("ao vivo que chega depois da hidratação continua contando como nova", () => {
    const store = createAppStore();
    store.hydrateDialog(PID, "s1", [row(1)]);
    const before = agentRows(store);
    const seen = before.reduce((max, m) => Math.max(max, m.seq ?? 0), 0);
    store.applyEvent(live("nova", BASE + 2));

    expect(countNewer(agentRows(store), seen)).toBe(1);
  });
});
