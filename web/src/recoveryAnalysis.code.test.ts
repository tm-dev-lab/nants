/*
 * Spec 133 T11 (D3): a análise de recuperação e o diagnóstico de ambiente chegam à conversa com
 * `code` e `params`; o título sai do dicionário pelo código e as linhas fixas pelo rótulo, nos dois
 * idiomas. O texto continua o da seção da tarefa (com os marcadores que o runner lê), em pt.
 */
import { describe, expect, it } from "vitest";
import { looksPortuguese } from "./i18n/ptLeftovers";
import { RECOVERY_ANALYSIS_CODE, RECOVERY_DIAGNOSIS_CODE, recoveryMessageText } from "./recoveryAnalysis";

/** A análise como o servidor a grava (recovery.rs): título e linhas fixas em pt-BR. */
const ANALYSIS = [
  "Análise de recuperação #2 (ESCALATE)",
  "- Causa raiz: limite de ciclos de recuperação atingido",
  "- Decisão: escalar ao humano",
  "- Estratégia sugerida: separate the websocket client",
  "- Contexto:",
  "  - comando: npm test",
  "  - código: 1",
  "  - trecho do erro:",
  "      (sem saída registrada)",
  "  - o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 3 rodadas autônomas",
  "  - por que não resolveu: limite de ciclos de recuperação atingido",
  "  - opções: (1) orientar o Desenvolvedor com uma estratégia concreta e usar Retomar; (2) revisar o objetivo/critérios da tarefa; (3) decidir o ponto de produto em aberto",
  "- Contagem: não conta como ciclo (escalada determinística)",
].join("\n");

const DIAGNOSIS = ["Diagnóstico de ambiente #1", "- Motivo: docker indisponível", "- Causa raiz: the daemon is down", "- O que fazer: start Docker"].join("\n");

describe("análise de recuperação com código (spec 133 T11)", () => {
  const analysis = { text: ANALYSIS, code: RECOVERY_ANALYSIS_CODE, params: { cycle: 2, decision: "ESCALATE", autonomous: 0 } };

  it("em en: o título pelo código e as linhas fixas traduzidas; o texto do agente como veio", () => {
    const text = recoveryMessageText(analysis, "en");
    const lines = text.split("\n");
    expect(lines[0]).toBe("Recovery analysis #2 (ESCALATE)");
    expect(text).toContain("separate the websocket client");
    expect(text).toContain("npm test");
    expect(looksPortuguese(text)).toBe(false);
  });

  it("em pt-BR: o mesmo texto do servidor", () => {
    expect(recoveryMessageText(analysis, "pt-BR")).toBe(ANALYSIS);
  });

  it("a rodada autônoma vem do params, com a marca no idioma", () => {
    const auto = { ...analysis, params: { cycle: 3, decision: "RETRY", autonomous: 1 } };
    expect(recoveryMessageText(auto, "en").split("\n")[0]).toBe("Recovery analysis #3 (RETRY) [autonomous round]");
    expect(recoveryMessageText(auto, "pt-BR").split("\n")[0]).toBe("Análise de recuperação #3 (RETRY) [rodada autônoma]");
  });

  it("o diagnóstico de ambiente sai traduzido nos dois idiomas", () => {
    const diagnosis = { text: DIAGNOSIS, code: RECOVERY_DIAGNOSIS_CODE, params: { n: 1 } };
    const en = recoveryMessageText(diagnosis, "en");
    expect(en.split("\n")).toEqual([
      "Environment diagnosis #1",
      "- Reason: docker unavailable",
      "- Root cause: the daemon is down",
      "- What to do: start Docker",
    ]);
    expect(recoveryMessageText(diagnosis, "pt-BR")).toBe(DIAGNOSIS);
  });

  it("sem código (registro antigo), pelo cabeçalho do texto, como antes", () => {
    expect(recoveryMessageText({ text: ANALYSIS }, "en").split("\n")[0]).toBe("Recovery analysis #2 (ESCALATE)");
    expect(recoveryMessageText({ text: "texto comum" }, "en")).toBe("texto comum");
  });
});
