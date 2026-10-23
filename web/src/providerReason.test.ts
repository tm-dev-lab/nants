/*
 * Spec 119: o motivo da falha do provedor (o detalhe entre parênteses) sai no idioma da tela em
 * todos os caminhos: conversa (502 do chat), mensagem do sistema da execução (cartão de pausa e
 * Quadro), Atividade (nova tentativa e falha da imagem) e registros antigos gravados em pt-BR.
 * Trava: em en, nenhum desses textos tem português.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { postChat, type ApiFailure } from "./api/client";
import { stepText, toolActionText } from "./apps/shared/activityText";
import { liveActionOf } from "./apps/activity/activityModel";
import { chatFailureMessage } from "./chatFailure";
import { localizeProviderDetail, providerFailureText, providerReasonText } from "./copy";
import { en } from "./i18n/en";
import { setLanguage } from "./i18n/index";
import { ptBR } from "./i18n/ptBR";
import { looksPortuguese } from "./i18n/ptLeftovers";
import { systemMessageText } from "./i18n/serverCodes";

/** Os tipos do motivo (`BadKind::as_str` e `http_code`), lidos do dicionário. */
const KINDS = Object.keys(ptBR)
  .filter((k) => k.startsWith("provider.kind."))
  .map((k) => k.slice("provider.kind.".length));

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

afterEach(() => {
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false, storage: null });
});

describe("motivo da falha do provedor no idioma da tela (spec 119)", () => {
  it("cada tipo tem rótulo nos dois idiomas, e o en não tem português", () => {
    expect(KINDS).toContain("no_final");
    expect(KINDS.length).toBeGreaterThanOrEqual(12);
    for (const kind of KINDS) {
      const key = `provider.kind.${kind}` as keyof typeof en;
      expect(Object.keys(en), key).toContain(key);
      expect(looksPortuguese(en[key]), key).toBe(false);
    }
  });

  it("conversa: o 502 do chat monta o motivo pelo kind, no idioma ativo", async () => {
    setLanguage("en", { persist: false, storage: null });
    for (const kind of KINDS) {
      vi.stubGlobal(
        "fetch",
        vi.fn(async () => jsonResponse(502, { error: "x", code: "provider_failed", params: { provider: "provider_bad_response", kind } })),
      );
      const result = await postChat("p1", "oi");
      if (result.ok) throw new Error("esperado erro");
      expect(looksPortuguese(result.error.message), result.error.message).toBe(false);
      const shown = chatFailureMessage(result.error, null);
      expect(looksPortuguese(shown), shown).toBe(false);
    }
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => jsonResponse(502, { error: "x", code: "provider_failed", params: { provider: "provider_bad_response", kind: "no_final" } })),
    );
    const result = await postChat("p1", "oi");
    if (result.ok) throw new Error("esperado erro");
    expect(result.error.message).toBe("The provider returned an unexpected response (stream without a final event).");
    expect(result.error.providerKind).toBe("no_final");
    // A mesma falha, exibida depois da troca de idioma, segue o idioma novo.
    setLanguage("pt-BR", { persist: false, storage: null });
    expect(chatFailureMessage(result.error, null)).toBe("O provedor devolveu uma resposta inesperada (transmissão sem evento final).");
  });

  it("conversa: limite do provedor pelo kind ganha a dica em en", () => {
    setLanguage("en", { persist: false, storage: null });
    const error: ApiFailure = {
      kind: "http",
      status: 502,
      message: "x",
      providerCode: "provider_bad_response",
      providerKind: "throttled",
      providerKindCode: "no_capacity",
    };
    const shown = chatFailureMessage(error, null);
    expect(shown).toContain("(provider at its limit: no_capacity)");
    expect(shown).toContain(en["providerMsg.throttleAdvice"]);
    expect(looksPortuguese(shown)).toBe(false);
  });

  it("conversa: detail antigo em pt-BR sai no idioma ativo", () => {
    setLanguage("en", { persist: false, storage: null });
    const error: ApiFailure = { kind: "http", status: 502, message: "x", providerCode: "provider_bad_response", providerDetail: "transmissão sem evento final" };
    expect(chatFailureMessage(error, null)).toBe("The provider returned an unexpected response (stream without a final event).");
  });

  it("execução (cartão de pausa e Quadro): a mensagem com code e o texto antigo saem em en", () => {
    setLanguage("en", { persist: false, storage: null });
    const coded = systemMessageText({
      text: "provider_bad_response (transmissão sem evento final)",
      code: "run.provider_failed_detail",
      params: { provider: "provider_bad_response", kind: "no_final" },
    });
    expect(coded).toBe("The model provider failed (provider_bad_response: stream without a final event).");
    const legacy = providerFailureText("provider_bad_response (incompleta: content_filter)", null);
    expect(legacy).toBe("The provider returned an unexpected response (incomplete: content_filter).");
    expect(providerFailureText("provider_http_400 (código: invalid_value)", null)).toContain("(code: invalid_value)");
  });

  it("atividade: nova tentativa com code e falha da imagem saem em en", () => {
    setLanguage("en", { persist: false, storage: null });
    const retry = {
      text: "resposta do provedor falhou (transmissão sem evento final); repetindo (1/2)",
      textCode: { code: "provider.retry", params: { kind: "no_final", attempt: 1, total: 2 } },
    };
    expect(stepText(retry)).toBe("the provider response failed (stream without a final event); retrying (1/2)");
    const live = liveActionOf({ agent: "developer", reasoning: "", text: retry.text, tool: "", textCode: retry.textCode });
    expect(live.kind).toBe("retry");
    expect(live.detail).toBe("the provider response failed (stream without a final event); retrying (1/2)");
    const image = toolActionText({
      seq: 1,
      ts: 0,
      agent: "developer",
      task_id: "T01",
      kind: "tool",
      text: "geração de imagem falhou (provider_bad_response (transmissão sem evento final))",
      code: "tool.image_failed",
      params: { provider: "provider_bad_response", kind: "no_final" },
    });
    expect(image).toBe("image generation failed (provider_bad_response (stream without a final event))");
    expect(looksPortuguese(image)).toBe(false);
  });

  it("trilha: falha da imagem gravada antes da 119 (só `reason`) sai em en; fora do vocabulário, como veio", () => {
    setLanguage("en", { persist: false, storage: null });
    const line = (reason: string) => ({
      seq: 1,
      ts: 0,
      agent: "developer",
      task_id: "T01",
      kind: "tool",
      text: `geração de imagem falhou (${reason})`,
      code: "tool.image_failed",
      params: { reason },
    });
    expect(toolActionText(line("provider_bad_response (transmissão sem evento final)"))).toBe(
      "image generation failed (provider_bad_response (stream without a final event))",
    );
    expect(toolActionText(line("provider_http_400 (código: invalid_value)"))).toBe("image generation failed (provider_http_400 (code: invalid_value))");
    expect(toolActionText(line("provider_timeout"))).toBe("image generation failed (provider_timeout)");
    expect(toolActionText(line("provider_bad_response (motivo desconhecido)"))).toBe(
      "image generation failed (provider_bad_response (motivo desconhecido))",
    );
    expect(toolActionText(line("algo fora do vocabulário"))).toBe("image generation failed (algo fora do vocabulário)");
    setLanguage("pt-BR", { persist: false, storage: null });
    expect(toolActionText(line("provider_bad_response (transmissão sem evento final)"))).toBe(
      "geração de imagem falhou (provider_bad_response (transmissão sem evento final))",
    );
  });

  it("leitura de volta: só o vocabulário; o resto fica como veio", () => {
    setLanguage("en", { persist: false, storage: null });
    expect(localizeProviderDetail("provedor no limite: no_capacity")).toBe("provider at its limit: no_capacity");
    expect(localizeProviderDetail("texto livre")).toBe("texto livre");
    expect(localizeProviderDetail("incompleta: com espaço")).toBe("incompleta: com espaço");
    expect(providerReasonText("desconhecido", null)).toBeNull();
    expect(providerReasonText("incomplete", "Com Espaço")).toBe("incomplete");
  });
});
