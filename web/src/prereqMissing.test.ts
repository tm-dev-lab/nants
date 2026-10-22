/*
 * 412 `prereq_missing` (spec 093 T02): o erro do chat e o do aprovar mostram o que falta
 * com rótulos fixos (e a aba de cada item); nada do corpo do servidor é exibido cru.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { approveSpec, postChat } from "./api/client";
import { chatFailureMessage } from "./chatFailure";
import { approveProblem } from "./apps/shared/runProblems";
import { prereqMissingMessage } from "./copy";
import { installFetch } from "./test-utils";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("mensagem do 412 prereq_missing", () => {
  it("lista os rótulos fixos na ordem, sem repetir e sem ids desconhecidos", () => {
    expect(prereqMissingMessage(["connection", "agent_architect"])).toBe(
      "Falta configurar: conexão com um provedor, Arquiteto.",
    );
    expect(prereqMissingMessage(["docker_daemon", "docker_daemon", "<script>"])).toBe(
      "Falta configurar: Docker daemon.",
    );
    expect(prereqMissingMessage([])).toBe("Falta configurar: pré-requisitos do Nants (veja as Configurações).");
  });

  it("o chat mostra o que falta configurar", async () => {
    installFetch(() => ({ status: 412, body: { error: "prereq_missing", missing: ["agent_architect"] } }));
    const result = await postChat("p1", "oi");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.status).toBe(412);
    expect(result.error.reason).toBe("prereq_missing");
    expect(result.error.missing).toEqual(["agent_architect"]);
    expect(chatFailureMessage(result.error, null)).toBe("Falta configurar: Arquiteto.");
  });

  it("o aprovar mostra o que falta e oferece Configurar quando faltam agentes", async () => {
    installFetch(() => ({
      status: 412,
      body: { error: "prereq_missing", missing: ["agent_developer", "docker_daemon", 42, "texto livre"] },
    }));
    const result = await approveSpec("p1", "s1");
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.error.missing).toEqual(["agent_developer", "docker_daemon"]);
    const problem = approveProblem(result.error);
    expect(problem.prereq).toBe(true);
    expect(problem.message).toMatch(/^Falta configurar: /);
    // Cada pendência leva à aba certa das Configurações ("Configurar").
    expect(problem.missing?.map((item) => [item.id, item.settings_tab])).toEqual([
      ["agent_developer", "agents"],
      ["docker_daemon", "machine"],
    ]);
  });

  it("só máquina pendente: tudo leva à aba Máquina, nada à de agentes", async () => {
    installFetch(() => ({ status: 412, body: { error: "prereq_missing", missing: ["git", "docker_cli"] } }));
    const result = await approveSpec("p1", "s1");
    if (result.ok) throw new Error("esperava falha");
    const problem = approveProblem(result.error);
    expect(problem.missing?.map((item) => item.settings_tab)).toEqual(["machine", "machine"]);
  });

  it("412 sem o corpo esperado segue o caminho genérico", async () => {
    installFetch(() => ({ status: 412, body: { error: "outra coisa" } }));
    const result = await approveSpec("p1", "s1");
    if (result.ok) throw new Error("esperava falha");
    expect(result.error.reason).toBeUndefined();
    expect(result.error.missing).toBeUndefined();
  });
});
