import { describe, expect, it } from "vitest";
import type { ApiFailure } from "./api/client";
import { chatFailureMessage } from "./chatFailure";
import { CHAT_TIMEOUT_DRAFT_SAVED_NOTICE, CHAT_TIMEOUT_NOTICE } from "./copy";

describe("504 do chat: o servidor cancelou o turno", () => {
  const timeout = (reason?: string): ApiFailure => ({
    kind: "http",
    status: 504,
    message: "timeout",
    ...(reason === undefined ? {} : { reason }),
  });

  it("sem reason: o aviso de turno cancelado", () => {
    expect(chatFailureMessage(timeout(), "chat")).toBe(CHAT_TIMEOUT_NOTICE);
  });

  it("reason draft_saved: avisa que o plano foi salvo", () => {
    expect(chatFailureMessage(timeout("draft_saved"), null)).toBe(CHAT_TIMEOUT_DRAFT_SAVED_NOTICE);
    expect(CHAT_TIMEOUT_DRAFT_SAVED_NOTICE).toContain("plano foi salvo");
  });

  it("outro reason segue o aviso de turno cancelado", () => {
    expect(chatFailureMessage(timeout("outro"), null)).toBe(CHAT_TIMEOUT_NOTICE);
  });
});

function providerFailure(providerCode: string, providerDetail?: string): ApiFailure {
  return {
    kind: "http",
    status: 502,
    message: "O provedor devolveu uma resposta inesperada.",
    providerCode,
    ...(providerDetail === undefined ? {} : { providerDetail }),
  };
}

describe("erro do chat com motivo do provedor (spec 088)", () => {
  it("usa o detail entre parênteses antes do ponto final", () => {
    expect(chatFailureMessage(providerFailure("provider_bad_response", "sem texto nem ação"), null)).toBe(
      "O provedor devolveu uma resposta inesperada (sem texto nem ação).",
    );
  });

  it("sem detail, a mensagem fica como antes", () => {
    expect(chatFailureMessage(providerFailure("provider_bad_response"), "chat")).toBe(
      "O provedor devolveu uma resposta inesperada.",
    );
  });

  it("detail inválido é ignorado", () => {
    expect(chatFailureMessage(providerFailure("provider_bad_response", "vazia\nsegredo"), null)).toBe(
      "O provedor devolveu uma resposta inesperada.",
    );
    expect(chatFailureMessage(providerFailure("provider_bad_response", "x".repeat(81)), null)).toBe(
      "O provedor devolveu uma resposta inesperada.",
    );
  });
});

describe("erro do chat no limite do provedor (spec 089)", () => {
  const LIMIT = "O provedor está limitando as requisições. O Nants já reduz o ritmo sozinho; continue em alguns minutos.";
  const ADVICE = "O Nants já reduz o ritmo sozinho; continue em alguns minutos.";

  it("provider_bad_response com detalhe de limite ganha a dica", () => {
    expect(chatFailureMessage(providerFailure("provider_bad_response", "provedor no limite: no_capacity"), null)).toBe(
      `O provedor devolveu uma resposta inesperada (provedor no limite: no_capacity). ${LIMIT}`,
    );
    expect(chatFailureMessage(providerFailure("provider_bad_response", "provedor no limite"), "chat")).toBe(
      `O provedor devolveu uma resposta inesperada (provedor no limite). ${LIMIT}`,
    );
  });

  it("429 traz o conselho de espera", () => {
    expect(chatFailureMessage(providerFailure("provider_http_429"), "chat")).toBe(
      `O provedor limitou as requisições (HTTP 429). ${ADVICE}`,
    );
  });

  it("demais falhas não ganham a dica", () => {
    for (const [code, detail] of [
      ["provider_bad_response", "sem texto nem ação"],
      ["provider_bad_response", undefined],
      ["provider_timeout", "provedor no limite"],
      ["provider_http_503", undefined],
    ] as const) {
      const text = chatFailureMessage(providerFailure(code, detail), "chat");
      expect(text).not.toContain(ADVICE);
    }
  });
});
