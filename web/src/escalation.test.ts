import { describe, expect, it } from "vitest";
import { parseEscalation, parseOptions } from "./escalation";

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
    const c = parseEscalation(FULL_ESCALATION);
    expect(c?.rootCause).toBe("porta ocupada");
    expect(c?.command).toBe("cargo test");
    expect(c?.code).toBe("101");
    expect(c?.excerpt).toBe("error: boom\n  at <b>x</b>");
    expect(c?.attempted).toContain("3 de 3 ciclos");
    expect(c?.whyStopped).toBe("o erro persistiu");
    expect(c?.options).toEqual(["liberar a porta", "trocar a porta", "abortar"]);
  });
  it("sem Contexto devolve null", () => {
    expect(parseEscalation("Causa raiz: X. Proposta: Y")).toBeNull();
  });
  it("tolera campos ausentes", () => {
    const c = parseEscalation("- Causa raiz: a\n- Contexto:\n  - comando: ls");
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
    const c = parseEscalation(text);
    expect(c?.rootCause).toBe("linha um\nlinha dois");
    expect(c?.command).toBe("cargo test\n--flag");
    expect(c?.whyStopped).toBe("motivo\n- x: y");
    expect(c?.options).toEqual(["a", "b"]);
  });
});
