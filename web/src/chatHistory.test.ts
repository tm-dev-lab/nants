/*
 * Carga do histórico do chat do Arquiteto para o store (`loadChatHistory` e `hydrateHistory`):
 * dedupe por turn_id com o que já veio por POST/SSE, mensagens otimistas preservadas, falhas
 * silenciosas que não apagam nada e respostas atrasadas que não vazam para outro projeto.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ChatMessage } from "./store";
import { appStore, createAppStore } from "./store";
import { loadChatHistory } from "./chatHistory";
import { installFetch, makeConfig, makeProject, resetStore, type RouteReply } from "./test-utils";

const PID = "p1";
const OTHER = "p2";
const TURN_1 = "0123456789abcdef0123456789abcdef";

beforeEach(() => resetStore({ config: makeConfig([makeProject(PID, "api-pagamentos"), makeProject(OTHER, "outro-projeto")]) }));
afterEach(() => vi.unstubAllGlobals());

const texts = (id = PID): string[] => (appStore.getState().messages[id] ?? []).map((m) => m.text);

function serveHistory(reply: (projectId: string) => RouteReply | Promise<RouteReply>) {
  return installFetch((_method, url) => reply(/\/api\/projects\/([^/]+)\/chat$/.exec(url)?.[1] ?? ""));
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve: (value: T) => void = () => undefined;
  const promise = new Promise<T>((res) => (resolve = res));
  return { promise, resolve };
}

describe("loadChatHistory", () => {
  it("otimista do usuário + turno persistido com o mesmo texto: as três mensagens aparecem, nenhuma perdida", async () => {
    resetStore({ messages: { [PID]: [{ id: 1, role: "user", text: "ok", at: 0 }] satisfies ChatMessage[] } });
    serveHistory(() => ({ body: { turns: [{ role: "user", text: "ok", ts: 1, turn_id: TURN_1 }, { role: "architect", text: "r1", ts: 2, turn_id: TURN_1 }] } }));
    await loadChatHistory(PID);
    expect(texts().sort()).toEqual(["ok", "ok", "r1"]);
  });

  it("turno já exibido por POST/SSE (turn_id conhecido) não entra de novo com o histórico", async () => {
    resetStore({
      messages: {
        [PID]: [
          { id: 1, role: "user", text: "oi", at: 0, confirmed: true, turnId: TURN_1 },
          { id: 2, role: "architect", agent: "architect", text: "Resposta", at: 0, origin: "live", turnId: TURN_1 },
        ] satisfies ChatMessage[],
      },
      shownTurns: { [PID]: [TURN_1] },
    });
    serveHistory(() => ({ body: { turns: [{ role: "user", text: "oi", ts: 1, turn_id: TURN_1 }, { role: "architect", text: "Resposta", ts: 2, turn_id: TURN_1 }] } }));
    await loadChatHistory(PID);
    expect(texts()).toEqual(["oi", "Resposta"]);
  });

  it("pergunta de outra aba (persistida) aparece mesmo com a resposta já exibida ao vivo", async () => {
    resetStore({
      messages: { [PID]: [{ id: 1, role: "architect", agent: "architect", text: "R", at: 0, origin: "live", turnId: TURN_1 }] satisfies ChatMessage[] },
      shownTurns: { [PID]: [TURN_1] },
    });
    serveHistory(() => ({ body: { turns: [{ role: "user", text: "Q", ts: 1, turn_id: TURN_1 }, { role: "architect", text: "R", ts: 2, turn_id: TURN_1 }] } }));
    await loadChatHistory(PID);
    expect(texts()).toEqual(["Q", "R"]);
  });

  it("mensagem otimista do usuário (sem confirmação) não some quando o histórico chega, e fica depois dele", async () => {
    const history = deferred<RouteReply>();
    serveHistory(() => history.promise);
    const pending = loadChatHistory(PID);
    appStore.appendMessage(PID, { role: "user", text: "nova" });
    history.resolve({ body: { turns: [{ role: "user", text: "antiga", ts: 1 }] } });
    await pending;
    expect(texts()).toEqual(["antiga", "nova"]);
  });

  it.each([
    ["HTTP 500", { status: 500, body: { error: "falha interna", code: "internal" } } as RouteReply],
    ["HTTP 404", { status: 404, body: { error: "projeto não encontrado", code: "project_not_found" } } as RouteReply],
  ])("falha do histórico (%s) não lança e não limpa a conversa já existente", async (_label, reply) => {
    resetStore({ messages: { [PID]: [{ id: 1, role: "user", text: "vivo", at: 0 }] satisfies ChatMessage[] } });
    serveHistory(() => reply);
    await expect(loadChatHistory(PID)).resolves.toBeUndefined();
    expect(texts()).toEqual(["vivo"]);
  });

  it("falha de rede no histórico não lança", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new TypeError("network"))));
    await expect(loadChatHistory(PID)).resolves.toBeUndefined();
    expect(appStore.getState().messages[PID]).toBeUndefined();
  });

  it("troca rápida de projeto: a resposta atrasada do projeto anterior não aparece no novo", async () => {
    const slow = deferred<RouteReply>();
    serveHistory((pid) => (pid === PID ? slow.promise : { body: { turns: [{ role: "architect", text: "Histórico de outro", ts: 2 }] } }));
    const first = loadChatHistory(PID);
    await loadChatHistory(OTHER);
    slow.resolve({ body: { turns: [{ role: "user", text: "Conversa do projeto anterior", ts: 1 }] } });
    await first;
    expect(texts(OTHER)).toEqual(["Histórico de outro"]);
    expect(texts(PID)).toEqual(["Conversa do projeto anterior"]);
  });

  it("só a carga mais nova do mesmo projeto é aplicada", async () => {
    const old = deferred<RouteReply>();
    let call = 0;
    serveHistory(() => (++call === 1 ? old.promise : { body: { turns: [{ role: "architect", text: "nova", ts: 2 }] } }));
    const first = loadChatHistory(PID);
    await loadChatHistory(PID);
    old.resolve({ body: { turns: [{ role: "architect", text: "velha", ts: 1 }] } });
    await first;
    expect(texts()).toEqual(["nova"]);
  });
});

describe("histórico substitui linhas persistidas; ao vivo só sobrevive o não persistido", () => {
  const H = [
    { role: "user" as const, text: "Quero X", ts: 1, turn_id: TURN_1 },
    { role: "architect" as const, text: "Entendido.", ts: 2, turn_id: TURN_1 },
  ];
  const storeTexts = (store: ReturnType<typeof createAppStore>): string[] => (store.getState().messages[PID] ?? []).map((m) => m.text);

  it("reabrir com histórico sem turn_id não cresce a lista", () => {
    const store = createAppStore();
    const legacy = [
      { role: "user" as const, text: "Quero X", ts: 1 },
      { role: "architect" as const, text: "Entendido.", ts: 2 },
    ];
    store.hydrateHistory(PID, legacy);
    store.hydrateHistory(PID, legacy);
    store.hydrateHistory(PID, legacy);
    expect(store.getState().messages[PID]).toHaveLength(2);
  });

  it("em voo + histórico sem o turno: otimista mantida no fim", () => {
    const store = createAppStore();
    store.appendMessage(PID, { role: "user", text: "nova" });
    store.hydrateHistory(PID, H);
    expect(storeTexts(store)).toEqual(["Quero X", "Entendido.", "nova"]);
  });

  it("confirmada com turn_id presente no histórico: aparece uma vez", () => {
    const store = createAppStore();
    const id = store.appendMessage(PID, { role: "user", text: "Quero X" });
    store.confirmUserMessage(PID, id, TURN_1);
    store.hydrateHistory(PID, H);
    expect(storeTexts(store)).toEqual(["Quero X", "Entendido."]);
  });

  it("em voo cujo turno já foi persistido: duplicata aparente aceita, nada perdido", () => {
    const store = createAppStore();
    store.appendMessage(PID, { role: "user", text: "Quero X" });
    store.hydrateHistory(PID, H);
    expect(storeTexts(store)).toEqual(["Quero X", "Entendido.", "Quero X"]);
  });

  it("resposta ao vivo com turn_id já no histórico sai (vem da linha persistida)", () => {
    const store = createAppStore();
    store.showArchitectReply(PID, "Entendido.", TURN_1);
    store.hydrateHistory(PID, H);
    expect(storeTexts(store)).toEqual(["Quero X", "Entendido."]);
  });
});
