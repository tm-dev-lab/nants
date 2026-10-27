// @vitest-environment node
/*
 * Spec 129 T04: as marcas estruturais gravadas pelo servidor no idioma do projeto são lidas nas
 * duas formas (pt-BR e en), pelas chaves do dicionário: a mensagem do Revisor (`reviewMessage.ts`)
 * e a análise de recuperação (`recoveryAnalysis.ts`).
 */
import { describe, expect, it } from "vitest";
import { en } from "./i18n/en";
import { ptBR } from "./i18n/ptBR";
import { finalVerificationCounts, markerForms, parseReviewMessage, splitCaptures, splitChecklist } from "./reviewMessage";
import { isRecoveryAnalysis, localizeAttempted, localizeRecoveryAnalysis, localizeRecoveryCause } from "./recoveryAnalysis";

/** A mesma mensagem do Revisor, gravada num projeto em pt e num em inglês (conteúdo igual). */
const REVIEW_PT =
  "Verificação final: 1 de 2 critérios atendidos\n\nPediu ajustes: Needs a guard\n\nApontamentos:\n- F-01 [HIGH, novo] src/a.ts:10: Missing guard → Add the guard\n  Cenário de falha: empty input → crash\n- F-04 [HIGH, baixa confiança, aberto há 3 revisões] src/d.ts:2: Old → Fix\n\nObservações (não bloqueiam):\n- F-02 [MEDIUM, observação (arquivo não mudou nesta rodada)] src/b.ts:1: Naming\n\nResolvidos:\n- F-03 [HIGH] src/c.ts:1: Done\n\nRespostas do Desenvolvedor:\n- F-03 [fixed] src/c.ts:1\n\nDecisões do Arquiteto:\n- F-05 [manual] title: reason\n\nCapturas avaliadas: t01-1-a.png\n\nCritérios:\n✓ Login works: test passes\n✗ Logout works: missing";
const REVIEW_EN =
  "Final verification: 1 of 2 criteria met\n\nChanges requested: Needs a guard\n\nFindings:\n- F-01 [HIGH, new] src/a.ts:10: Missing guard → Add the guard\n  Failure scenario: empty input → crash\n- F-04 [HIGH, low confidence, open for 3 reviews] src/d.ts:2: Old → Fix\n\nObservations (non-blocking):\n- F-02 [MEDIUM, observation (file did not change in this round)] src/b.ts:1: Naming\n\nResolved:\n- F-03 [HIGH] src/c.ts:1: Done\n\nDeveloper responses:\n- F-03 [fixed] src/c.ts:1\n\nArchitect decisions:\n- F-05 [manual] title: reason\n\nScreenshots reviewed: t01-1-a.png\n\nCriteria:\n✓ Login works: test passes\n✗ Logout works: missing";

describe("mensagem do Revisor nas duas formas (spec 129 T04)", () => {
  it("as marcas vêm do dicionário, uma por idioma", () => {
    expect(markerForms("marker.findings")).toEqual([ptBR["marker.findings"], en["marker.findings"]]);
    expect(markerForms("marker.criteria")).toEqual(["Critérios", "Criteria"]);
  });

  it("lê a mensagem em pt e em en com o mesmo resultado", () => {
    const pt = parseReviewMessage(REVIEW_PT);
    const english = parseReviewMessage(REVIEW_EN);
    expect(pt).not.toBeNull();
    expect(english).toEqual({ ...pt, prefix: "Final verification: 1 of 2 criteria met" });
    expect(english?.verdict).toBe("changes");
    expect(english?.summary).toBe("Needs a guard");
    expect(english?.findings).toEqual([
      { id: "F-01", severity: "HIGH", location: "src/a.ts:10", description: "Missing guard", recommendation: "Add the guard", scenario: "empty input → crash", state: "new" },
      { id: "F-04", severity: "HIGH", location: "src/d.ts:2", description: "Old", recommendation: "Fix", lowConfidence: true, state: "open", recurrence: 3 },
    ]);
    expect(english?.observations[0]).toMatchObject({ id: "F-02", severity: "MEDIUM", unchangedFile: true });
    expect(english?.resolved).toEqual([{ id: "F-03", severity: "HIGH", location: "src/c.ts:1", description: "Done", recommendation: "", state: "resolved" }]);
    expect(english?.responses).toEqual([{ id: "F-03", kind: "fixed", detail: "src/c.ts:1" }]);
    expect(english?.decisions).toEqual([{ id: "F-05", decision: "manual", detail: "title: reason" }]);
    expect(english?.captures).toEqual(["t01-1-a.png"]);
    expect(english?.items).toEqual([
      { met: true, text: "Login works: test passes" },
      { met: false, text: "Logout works: missing" },
    ]);
  });

  it("aprovação em en, sem diferenciar maiúsculas na marca", () => {
    const parsed = parseReviewMessage("APPROVED: all good\n\nCRITERIA:\n✓ a: b");
    expect(parsed?.verdict).toBe("approved");
    expect(parsed?.summary).toBe("all good");
    expect(parsed?.items).toEqual([{ met: true, text: "a: b" }]);
  });

  it("a marca só vale na linha inteira: a prosa que cita a palavra não abre seção", () => {
    const parsed = parseReviewMessage("Approved: the findings: none\nResolved: everything\n\nCriteria:\n✓ a");
    expect(parsed?.summary).toBe("the findings: none\nResolved: everything");
    expect(parsed?.findings).toEqual([]);
    expect(parsed?.resolved).toEqual([]);
    expect(splitChecklist("We met the criteria: all of them\n✓ a")).toBeNull();
    expect(splitCaptures("Approved: x\n\nScreenshots reviewed: a.png\nmore").captures).toEqual([]);
  });

  it("contagem da verificação final nas duas formas", () => {
    expect(finalVerificationCounts("Verificação final: 3 de 4 critérios atendidos")).toEqual({ met: 3, total: 4 });
    expect(finalVerificationCounts("Final verification: 3 of 4 criteria met")).toEqual({ met: 3, total: 4 });
    expect(finalVerificationCounts("Final verification: three of four")).toBeNull();
  });
});

const RECOVERY_PT =
  "Análise de recuperação #2 (ESCALATE)\n- Causa raiz: limite de ciclos de recuperação atingido\n- Decisão: escalar ao humano\n- Estratégia sugerida: Free port 3000\n- Contexto:\n  - comando: npm test\n  - o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas\n  - por que não resolveu: (citada pelo modelo) The port stays busy\n- Rodada autônoma (aprovação automática) #1 de 2\n- Contagem: não conta como ciclo (escalada determinística)";
const RECOVERY_EN =
  "Recovery analysis #2 (ESCALATE)\n- Root cause: recovery cycle limit reached\n- Decision: escalate to the human\n- Suggested strategy: Free port 3000\n- Context:\n  - command: npm test\n  - what was tried: 2 of 2 recovery cycles and 0 of 2 autonomous rounds\n  - why it was not solved: (cited by the model) The port stays busy\n- Autonomous round (auto-approve) #1 of 2\n- Count: does not count as a cycle (deterministic escalation)";

describe("análise de recuperação nas duas formas (spec 129 T04)", () => {
  it("pt vira en e en vira pt; no mesmo idioma volta idêntica", () => {
    expect(localizeRecoveryAnalysis(RECOVERY_PT, "en")).toBe(RECOVERY_EN);
    expect(localizeRecoveryAnalysis(RECOVERY_EN, "pt-BR")).toBe(RECOVERY_PT);
    expect(localizeRecoveryAnalysis(RECOVERY_PT, "pt-BR")).toBe(RECOVERY_PT);
    expect(localizeRecoveryAnalysis(RECOVERY_EN, "en")).toBe(RECOVERY_EN);
    expect(localizeRecoveryAnalysis("Free text from the agent", "pt-BR")).toBe("Free text from the agent");
  });

  it("reconhece o cabeçalho, a contagem e as causas fixas nas duas formas", () => {
    expect(isRecoveryAnalysis(RECOVERY_PT)).toBe(true);
    expect(isRecoveryAnalysis(RECOVERY_EN)).toBe(true);
    expect(isRecoveryAnalysis("Answer to the developer: ok")).toBe(false);
    expect(localizeAttempted("2 of 2 recovery cycles and 0 of 2 autonomous rounds", "pt-BR")).toBe(
      "2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas",
    );
    expect(localizeRecoveryCause("docker unavailable: no socket", "pt-BR")).toBe("docker indisponível: no socket");
    expect(localizeRecoveryCause("(citada pelo modelo) x", "en")).toBe("(cited by the model) x");
  });

  it("a forma antiga da rodada autônoma (YOLO) continua legível", () => {
    const legacy = "Análise de recuperação #1 (RETRY)\n- Rodada autônoma (YOLO) #2 de 3";
    expect(localizeRecoveryAnalysis(legacy, "en")).toBe("Recovery analysis #1 (RETRY)\n- Autonomous round (auto-approve) #2 of 3");
  });
});
