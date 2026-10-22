/* Verificação final na janela Conversa (spec 093, T06): a faixa enquanto o Revisor confere. */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { parseApiEvent } from "../../api/events";
import { Conversation } from "../conversation/Conversation";
import { appStore } from "../../store";
import type { ApiEvent } from "../../api/types";
import { installFetch, makeSpec, makeTask, resetStore } from "../../test-utils";

const PID = "p1";

function activity(payload: Record<string, unknown>): ApiEvent {
  const ev = parseApiEvent({ event_type: "agent_activity", project_id: PID, payload });
  if (ev === null) throw new Error("evento inválido");
  return ev;
}

const START = { agent: "reviewer", activity_id: "f1", kind: "start", stage: "final_verification" };

describe("verificação final: faixa na Conversa", () => {
  beforeEach(() => {
    resetStore({ activities: {} });
    installFetch(async () => ({ body: {} }));
  });
  afterEach(cleanup);

  function mount(): void {
    const spec = makeSpec("s1", "Plano", "IN_PROGRESS", [makeTask("T01", "A", "DONE")]);
    render(<Conversation projectId={PID} spec={spec} messages={[]} onDraftUpdated={async () => undefined} />);
  }

  it("mostra título e explicação enquanto a atividade existe e some ao terminar", () => {
    mount();
    expect(screen.queryByTestId("final-verification-strip")).toBeNull();
    act(() => appStore.applyEvent(activity(START)));
    const strip = screen.getByTestId("final-verification-strip");
    expect(strip.textContent).toContain("Verificação final em andamento");
    expect(strip.textContent).toContain("o Revisor confere a spec inteira; pode criar uma tarefa de correção");
    act(() => appStore.applyEvent(activity({ ...START, kind: "end" })));
    expect(screen.queryByTestId("final-verification-strip")).toBeNull();
  });

  it("outro stage não aciona o indicador", () => {
    mount();
    act(() => appStore.applyEvent(activity({ agent: "architect", activity_id: "a1", kind: "start", stage: "Escrevendo relatório final" })));
    expect(screen.queryByTestId("final-verification-strip")).toBeNull();
  });
});
