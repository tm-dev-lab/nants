import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { parseEscalation, parseOptions } from "./escalation";
import { en } from "./i18n/en";
import { looksPortuguese } from "./i18n/ptLeftovers";
import { ptBR } from "./i18n/ptBR";

export const FULL_ESCALATION = [
  "Análise de recuperação #3 (ESCALATE)",
  "- Causa raiz: porta ocupada",
  "- Decisão: escalar ao humano",
  "- Contexto:",
  "  - comando: cargo test",
  "  - código: 101",
  "  - trecho do erro:",
  "      error: boom",
  "        at <b>x</b>",
  "  - o que foi tentado: 3 de 3 ciclos de recuperação e 2 de 2 rodadas autônomas",
  "  - por que não resolveu: o erro persistiu",
  "  - opções: (1) liberar a porta; (2) trocar a porta; (3) abortar",
].join("\n");

describe("parseEscalation", () => {
  it("extrai todos os campos", () => {
    const c = parseEscalation(FULL_ESCALATION, "pt-BR");
    expect(c?.rootCause).toBe("porta ocupada");
    expect(c?.command).toBe("cargo test");
    expect(c?.code).toBe("101");
    expect(c?.excerpt).toBe("error: boom\n  at <b>x</b>");
    expect(c?.attempted).toContain("3 de 3 ciclos");
    expect(c?.whyStopped).toBe("o erro persistiu");
    expect(c?.options).toEqual(["liberar a porta", "trocar a porta", "abortar"]);
  });
  it("ASK: a pergunta do Arquiteto vai para o cartão (spec 124 T03)", () => {
    const c = parseEscalation(
      [
        "Análise de recuperação #2 (ASK)",
        "- Causa raiz: falta decidir",
        "- Decisão: pergunta ao humano (ASK)",
        "- Pergunta ao humano: As capturas podem ser manuais?",
        "  em 1920x1080",
        "- Contexto:",
        "  - opções: responda à pergunta acima no campo de orientação; a tarefa volta a executar com a sua resposta",
      ].join("\n"),
      "pt-BR",
    );
    expect(c?.question).toBe("As capturas podem ser manuais?\nem 1920x1080");
    expect(c?.rootCause).toBe("falta decidir");
    expect(parseEscalation(FULL_ESCALATION, "pt-BR")?.question).toBeNull();
  });
  it("sem Contexto devolve null", () => {
    expect(parseEscalation("Causa raiz: X. Proposta: Y", "pt-BR")).toBeNull();
  });
  it("tolera campos ausentes", () => {
    const c = parseEscalation("- Causa raiz: a\n- Contexto:\n  - comando: ls", "pt-BR");
    expect(c?.command).toBe("ls");
    expect(c?.excerpt).toBeNull();
    expect(c?.options).toEqual([]);
  });
  it("opções sem marcadores", () => {
    expect(parseOptions("tentar de novo")).toEqual(["tentar de novo"]);
  });
  it("valores de várias linhas (continuação com \n  )", () => {
    const text = [
      "- Causa raiz: linha um",
      "  linha dois",
      "- Decisão: escalar ao humano",
      "- Contexto:",
      "  - comando: cargo test",
      "  --flag",
      "  - código: 1",
      "  - por que não resolveu: motivo",
      "  - x: y",
      "  - opções: (1) a; (2) b",
    ].join("\n");
    const c = parseEscalation(text, "pt-BR");
    expect(c?.rootCause).toBe("linha um\nlinha dois");
    expect(c?.command).toBe("cargo test\n--flag");
    expect(c?.whyStopped).toBe("motivo\n- x: y");
    expect(c?.options).toEqual(["a", "b"]);
  });
});

/** Linhas `  - opções: ...` fixas do servidor, lidas do código Rust (com a continuação `\` + quebra + recuo). */
function serverOptionLines(): string[] {
  const read = (file: string) => readFileSync(resolve(process.cwd(), "..", file), "utf8");
  const source = read("src/server/recovery.rs") + read("src/agents/recovery_plan.rs");
  return [...source.matchAll(/"(  - opções: (?:[^"\\]|\\[\s\S])*)"/g)].map((m) => (m[1] ?? "").replace(/\\\n\s*/g, ""));
}

/** Bloco "Contexto" como o servidor grava hoje (`escalation_context`), com a linha de opções `options`. */
function serverBlock(options: string, code = "1", tail = "      error: boom"): string {
  return [
    "Análise de recuperação #2 (ESCALATE)",
    "- Causa raiz: limite de ciclos de recuperação atingido",
    "- Decisão: escalar ao humano",
    "- Contexto:",
    "  - comando: npm test",
    `  - código: ${code}`,
    "  - trecho do erro:",
    tail,
    "  - o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas",
    "  - por que não resolveu: limite de ciclos de recuperação atingido",
    options,
    "- Contagem: não conta como ciclo (escalada determinística)",
  ].join("\n");
}

const valueOf = (line: string) => line.slice(line.indexOf(": ") + 2);

describe("parseEscalation no idioma da tela (spec 142 T07)", () => {
  it("as quatro linhas de opções fixas do servidor existem e saem em inglês na tela em inglês", () => {
    const lines = serverOptionLines();
    expect(lines.length).toBe(4);
    for (const line of lines) {
      const c = parseEscalation(serverBlock(line), "en");
      expect(c?.options.length, line).toBeGreaterThan(0);
      for (const option of c?.options ?? []) expect(looksPortuguese(option), option).toBe(false);
      expect(c?.attempted).toBe("2 of 2 recovery cycles and 0 of 2 autonomous rounds");
      expect(c?.whyStopped).toBe("recovery cycle limit reached");
      expect(c?.command).toBe("npm test");
      expect(c?.excerpt).toBe("error: boom");
    }
  });

  it("a linha das opções propostas (a do achado) vira o texto do dicionário nos dois idiomas", () => {
    const line = `  ${ptBR["recoveryText.options.proposed"]}`;
    expect(parseEscalation(serverBlock(line), "en")?.options).toEqual([valueOf(en["recoveryText.options.proposed"])]);
    expect(parseEscalation(serverBlock(line), "pt-BR")?.options).toEqual([valueOf(ptBR["recoveryText.options.proposed"])]);
    const ask = parseEscalation(serverBlock(`  ${ptBR["recoveryText.options.ask"]}`), "en");
    expect(ask?.options).toEqual([valueOf(en["recoveryText.options.ask"])]);
  });

  it("as opções numeradas fixas viram itens traduzidos; as do modelo ficam como vieram", () => {
    const quality = parseEscalation(serverBlock(`  ${ptBR["recoveryText.options.quality"]}`), "en");
    expect(quality?.options).toEqual([
      "guide the Developer with a concrete strategy and use Resume",
      "revise the task objective/criteria",
      "decide the open product question",
    ]);
    const model = parseEscalation(serverBlock("  - opções: (1) liberar a porta; (2) abortar"), "en");
    expect(model?.options).toEqual(["liberar a porta", "abortar"]);
  });

  it("código de saída e saída vazia fixos do servidor saem no idioma da tela", () => {
    const timeout = parseEscalation(serverBlock("  - opções: x", "tempo limite", "      (sem saída registrada)"), "en");
    expect(timeout?.code).toBe("time limit");
    expect(timeout?.excerpt).toBe("(no output recorded)");
    expect(parseEscalation(serverBlock("  - opções: x", "não informado"), "en")?.code).toBe("not reported");
    expect(parseEscalation(serverBlock("  - opções: x", "127"), "en")?.code).toBe("127");
    expect(parseEscalation(serverBlock("  - opções: x", "tempo limite"), "pt-BR")?.code).toBe("tempo limite");
  });

  it("o bloco gravado em inglês também é lido, e volta em pt-BR na tela em português", () => {
    const english = [
      "Recovery analysis #2 (ESCALATE)",
      "- Root cause: recovery cycle limit reached",
      "- Context:",
      "  - command: npm test",
      "  - exit code: time limit",
      "  - error excerpt:",
      "      (no output recorded)",
      "  - what was tried: 2 of 2 recovery cycles and 0 of 2 autonomous rounds",
      "  - why it was not solved: recovery cycle limit reached",
      `  ${en["recoveryText.options.proposed"]}`,
      "- Count: does not count as a cycle (deterministic escalation)",
    ].join("\n");
    const pt = parseEscalation(english, "pt-BR");
    expect(pt?.rootCause).toBe("recovery cycle limit reached");
    expect(pt?.command).toBe("npm test");
    expect(pt?.code).toBe("tempo limite");
    expect(pt?.excerpt).toBe("(sem saída registrada)");
    expect(pt?.attempted).toBe("2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas");
    expect(pt?.whyStopped).toBe("limite de ciclos de recuperação atingido");
    expect(pt?.options).toEqual([valueOf(ptBR["recoveryText.options.proposed"])]);
    expect(parseEscalation(english, "en")?.options).toEqual([valueOf(en["recoveryText.options.proposed"])]);
  });

  it("registro antigo em pt-BR na tela em pt-BR volta igual", () => {
    const c = parseEscalation(serverBlock(`  ${ptBR["recoveryText.options.environmental"]}`), "pt-BR");
    expect(c?.options[0]).toMatch(/^ajustar `test_command`/);
    expect(c?.options.length).toBe(4);
    expect(c?.attempted).toBe("2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas");
  });
});
