import { describe, expect, it } from "vitest";
import type { ApiEvent } from "./api/types";
import { createAppStore, reduceEvent } from "./store";

/** Mensagem `agent_message` do sistema com o texto dado (código do servidor ou texto livre). */
function systemMessage(text: string): ApiEvent {
  return {
    event_type: "agent_message",
    project_id: "p1",
    payload: { agent: "system", text },
  } as unknown as ApiEvent;
}

function texts(event: ApiEvent): string[] {
  const state = createAppStore().getState();
  const patch = reduceEvent(state, event, 1);
  return (patch.messages?.p1 ?? []).map((m) => m.text);
}

describe("falha do provedor na execução", () => {
  it("código fixo do provedor vira a mensagem específica", () => {
    expect(texts(systemMessage("provider_http_400"))).toEqual([
      "O provedor recusou a requisição (HTTP 400). Verifique o modelo e o protocolo da conexão.",
    ]);
  });

  it("código de timeout vira a mensagem específica", () => {
    expect(texts(systemMessage("provider_timeout"))).toEqual(["O provedor não respondeu a tempo."]);
  });

  it("texto fora do vocabulário passa sem alteração (mensagens do sistema já existentes)", () => {
    const text = "a execução foi interrompida por erro interno; confira o estado do projeto";
    expect(texts(systemMessage(text))).toEqual([text]);
  });
});
