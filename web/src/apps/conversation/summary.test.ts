/* Subtítulo vivo e selo da Conversa (spec 093, T06): função pura. */
import { describe, expect, it } from "vitest";
import type { SpaceData } from "../../desktop/useSpaceData";
import { notReady, READY } from "../../desktop/shell-test-utils";
import { conversationSummary } from "./summary";

function space(extra: Partial<SpaceData> = {}): SpaceData {
  return { permissions: [], architectTurnActive: false, phase: "planning", planStage: null, ...extra } as SpaceData;
}

const PERMISSION = { requestId: "r1", projectId: "p1", taskId: null, agent: "architect", command: "ls" };

describe("conversationSummary", () => {
  it("sem espaço: nada", () => {
    expect(conversationSummary({ space: null, readiness: READY, projectCount: 0 })).toEqual({});
  });

  it("subtítulo: prontidão, pedidos, perguntas, pensando e a fase", () => {
    expect(conversationSummary({ space: space(), readiness: notReady(), projectCount: 1 }).subtitle).toBe("configure para conversar");
    expect(conversationSummary({ space: space({ permissions: [PERMISSION] }), readiness: READY, projectCount: 1 }).subtitle).toBe("1 pedido de permissão");
    expect(
      conversationSummary({ space: space(), readiness: READY, projectCount: 1, attention: { project_id: "p1", name: "Loja", pending_permissions: [], pending_choices: true, plan_awaiting_approval: false, paused_or_stalled: null } })
        .subtitle,
    ).toBe("perguntas do Arquiteto");
    expect(conversationSummary({ space: space({ architectTurnActive: true }), readiness: READY, projectCount: 1 }).subtitle).toBe("Arquiteto pensando");
    expect(conversationSummary({ space: space(), readiness: READY, projectCount: 1 }).subtitle).toMatch(/^Arquiteto · /);
  });

  it("etapa do plano (094): turno aberto no plano é 'planejando', não 'pensando'; revisando e aguardando pela fase", () => {
    const planning = space({ architectTurnActive: true, planStage: "planning", phase: "planning" });
    expect(conversationSummary({ space: planning, readiness: READY, projectCount: 1 }).subtitle).toBe("Arquiteto · planejando");
    const reviewing = space({ planStage: "reviewing", phase: "reviewing_plan" });
    expect(conversationSummary({ space: reviewing, readiness: READY, projectCount: 1 }).subtitle).toBe("Arquiteto · revisando o plano");
    const awaiting = space({ planStage: "awaiting", phase: "waiting_approval" });
    expect(conversationSummary({ space: awaiting, readiness: READY, projectCount: 1 }).subtitle).toBe("Arquiteto · aguardando sua aprovação");
  });

  it("selo: pendências da Conversa mais as mensagens novas", () => {
    expect(conversationSummary({ space: space(), readiness: READY, projectCount: 1 }).badge).toBeUndefined();
    expect(conversationSummary({ space: space({ permissions: [PERMISSION] }), readiness: READY, projectCount: 1, conversationUnseen: 2 }).badge).toEqual({
      count: 3,
      label: "1 pendência, 2 mensagens novas",
    });
    expect(conversationSummary({ space: space(), readiness: READY, projectCount: 1, conversationUnseen: 1 }).badge).toEqual({ count: 1, label: "1 mensagem nova" });
  });
});
