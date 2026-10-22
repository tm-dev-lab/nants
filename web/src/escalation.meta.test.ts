import { describe, expect, it } from "vitest";
import { parseEscalation } from "./escalation";

describe("parseEscalation: linhas de metadados", () => {
  it("não anexa Contagem, Rodada autônoma nem Correção de ambiente à última opção", () => {
    const text = [
      "- Causa raiz: x",
      "- Contexto:",
      "  - opções: (1) a; (2) b",
      "- Contagem: não conta como ciclo (escalada determinística)",
      "- Rodada autônoma 2 de 2",
      "- Correção de ambiente: tentada",
    ].join("\n");
    expect(parseEscalation(text)?.options).toEqual(["a", "b"]);
  });

  it("metadados no meio não vazam para o campo anterior", () => {
    const text = ["- Contexto:", "  - comando: ls", "- Contagem: x", "  - código: 2"].join("\n");
    const c = parseEscalation(text);
    expect(c?.command).toBe("ls");
    expect(c?.code).toBe("2");
  });
});
