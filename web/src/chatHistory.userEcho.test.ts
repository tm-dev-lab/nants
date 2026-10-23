/*
 * 114/T03 (achado 10): a mensagem do usuário aparece uma vez só na conversa, do envio à
 * confirmação, com ou sem anexo, mesmo quando o histórico é relido no meio do turno (fim de etapa,
 * reconexão do stream, retomada da aba ou watchdog).
 *
 * Causa reproduzida aqui: o servidor grava a mensagem do usuário no INÍCIO do turno. Uma releitura
 * do histórico durante o turno trazia a linha persistida, e `hydrateHistory` mantinha também o eco
 * otimista do composer, que ainda não estava confirmado (o POST só volta no fim do turno): duas
 * bolhas iguais até o POST voltar e a próxima releitura descartar o eco.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "./api/types";
import { appStore } from "./store";
import { loadChatHistory } from "./chatHistory";
import { installFetch, makeConfig, makeProject, resetStore } from "./test-utils";

const PID = "p1";
const TURN = "9172959a7d41c77c3e025eaeee4edeb2";
const TEXT = "Consegue mudar o laytout para deixar parecido com este da conversa?";
const IMAGE = "1b17730a6f62cfd326773c9dd43fc78b.png";

beforeEach(() => resetStore({ config: makeConfig([makeProject(PID, "test-01")]) }));
afterEach(() => vi.unstubAllGlobals());

const userTexts = (): string[] => (appStore.getState().messages[PID] ?? []).filter((m) => m.role === "user").map((m) => m.text);
const user = () => (appStore.getState().messages[PID] ?? []).filter((m) => m.role === "user");

/** Eco otimista do composer, como `Conversation.submit` o grava. */
function sendFromComposer(text: string, attachments: readonly string[] = []): number {
  return appStore.appendMessage(PID, { role: "user", text, inFlight: true, ...(attachments.length > 0 ? { attachments } : {}) });
}

function turnStarted(turnId: string, text: string): void {
  appStore.applyEvent({
    event_type: "chat_turn_started",
    project_id: PID,
    payload: { project_id: PID, turn_id: turnId, user_text: text, started_at: 1 },
  } as ApiEvent);
}

/** Linha do `.chat.jsonl` como o servidor a devolve, gravada no início do turno. */
function persistedUser(turnId: string, text: string, attachments: string[] = []) {
  return {
    role: "user" as const,
    text,
    ts: Math.floor(Date.now() / 1000),
    turn_id: turnId,
    ...(attachments.length > 0 ? { attachments } : {}),
  };
}

/** O POST do chat volta no fim do turno: confirma o eco e mostra a resposta. */
function postReturns(messageId: number, turnId: string, reply: string): void {
  appStore.settleUserMessage(PID, messageId);
  appStore.confirmUserMessage(PID, messageId, turnId);
  appStore.showArchitectReply(PID, reply, turnId);
}

describe("mensagem do usuário sem duplicar", () => {
  it("com anexo: releitura do histórico no meio do turno (etapa de revisão do plano) não duplica", () => {
    const id = sendFromComposer(TEXT, [IMAGE]);
    turnStarted(TURN, TEXT);
    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE])]);
    expect(userTexts()).toEqual([TEXT]);
    expect(user()[0]?.attachments).toEqual([IMAGE]);

    // Outra releitura (ping/watchdog) e o fim do turno: continua uma só.
    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE])]);
    postReturns(id, TURN, "Corrected T01.");
    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE]), { role: "architect", text: "Corrected T01.", ts: 2, turn_id: TURN }]);
    expect(userTexts()).toEqual([TEXT]);
    expect((appStore.getState().messages[PID] ?? []).map((m) => m.text)).toEqual([TEXT, "Corrected T01."]);
  });

  it("sem anexo: a mesma releitura no meio do turno não duplica", () => {
    sendFromComposer("monte o plano");
    turnStarted("t1", "monte o plano");
    appStore.hydrateHistory(PID, [persistedUser("t1", "monte o plano")]);
    expect(userTexts()).toEqual(["monte o plano"]);
  });

  it("reconexão: o início do turno se perdeu no stream e a ressincronização traz a linha persistida", async () => {
    const id = sendFromComposer(TEXT, [IMAGE]);
    // O `chat_turn_started` caiu na queda do stream: o eco não tem `turn_id`.
    installFetch(() => ({ body: { turns: [persistedUser(TURN, TEXT, [IMAGE])] } }));
    await loadChatHistory(PID);
    expect(userTexts()).toEqual([TEXT]);

    // O estado relido reaplica o turno ativo (syncArchitectTurn) e o evento chega repetido.
    appStore.syncArchitectTurn(PID, { turn_id: TURN, user_text: TEXT, started_at: 1 });
    turnStarted(TURN, TEXT);
    await loadChatHistory(PID);
    expect(userTexts()).toEqual([TEXT]);

    postReturns(id, TURN, "ok");
    expect(userTexts()).toEqual([TEXT]);
  });

  it("reconexão com o início do turno recebido: releituras repetidas não duplicam", async () => {
    sendFromComposer(TEXT, [IMAGE]);
    turnStarted(TURN, TEXT);
    installFetch(() => ({ body: { turns: [persistedUser(TURN, TEXT, [IMAGE])] } }));
    await loadChatHistory(PID);
    await loadChatHistory(PID);
    turnStarted(TURN, TEXT);
    expect(userTexts()).toEqual([TEXT]);
  });

  it("nada se perde: eco ainda não persistido continua no fim", () => {
    sendFromComposer("nova", [IMAGE]);
    appStore.hydrateHistory(PID, [persistedUser("t0", "antiga")]);
    expect(userTexts()).toEqual(["antiga", "nova"]);
  });

  it("nada se perde: mesmo texto de um turno já respondido não absorve o eco novo", () => {
    const first = sendFromComposer("ok");
    turnStarted("t1", "ok");
    postReturns(first, "t1", "r1");
    sendFromComposer("ok");
    appStore.hydrateHistory(PID, [persistedUser("t1", "ok"), { role: "architect", text: "r1", ts: 2, turn_id: "t1" }]);
    expect(userTexts()).toEqual(["ok", "ok"]);
  });

  it("releitura no meio do turno não esconde a resposta do Arquiteto, e a releitura final não a duplica", () => {
    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE])]);
    expect(appStore.showArchitectReply(PID, "R", TURN)).toBe(true);
    expect(appStore.showArchitectReply(PID, "R", TURN)).toBe(false);
    const architect = () => (appStore.getState().messages[PID] ?? []).filter((m) => m.role === "architect").map((m) => m.text);
    expect(architect()).toEqual(["R"]);

    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE]), { role: "architect", text: "R", ts: 2, turn_id: TURN }]);
    expect(architect()).toEqual(["R"]);
    expect(userTexts()).toEqual([TEXT]);
    expect(appStore.showArchitectReply(PID, "R", TURN)).toBe(false);
    expect(architect()).toEqual(["R"]);
  });

  it("do envio ao fim com releitura no meio: uma mensagem do usuário e uma resposta", () => {
    const id = sendFromComposer(TEXT, [IMAGE]);
    turnStarted(TURN, TEXT);
    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE])]);
    postReturns(id, TURN, "Corrected T01.");
    expect((appStore.getState().messages[PID] ?? []).map((m) => m.text)).toEqual([TEXT, "Corrected T01."]);
  });

  it("nada se perde: mesmo texto com outro anexo não absorve o eco", () => {
    sendFromComposer(TEXT, ["outra.png"]);
    appStore.hydrateHistory(PID, [persistedUser(TURN, TEXT, [IMAGE])]);
    expect(userTexts()).toEqual([TEXT, TEXT]);
  });
});
