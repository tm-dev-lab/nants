/*
 * Avisos do consenso do plano na Conversa (spec 131 T03): a rodada sem ponto em aberto (inclusive
 * o ajuste deduzido da mudança da tarefa) e a lista final do plano saem no tom neutro; a rodada
 * com ponto sem resposta continua como alerta. A lista final vem como conteúdo, depois da frase
 * fixa traduzida.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { Conversation } from "./Conversation";
import { appStore, useAppStore, type ChatMessage } from "../../store";
import { setLanguage } from "../../i18n";
import { systemMessageText } from "../../i18n/serverCodes";
import type { ChatHistoryTurn } from "../../api/types";

vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));
vi.mock("../../chatHistory", () => ({ loadChatHistory: vi.fn(async () => {}) }));

const NONE: ChatMessage[] = [];

afterEach(() => {
  cleanup();
  setLanguage("pt-BR");
});

function Harness({ id }: { id: string }) {
  const messages = useAppStore((s) => s.messages[id] ?? NONE);
  return <Conversation projectId={id} messages={messages} onDraftUpdated={async () => {}} />;
}

function notice(ts: number, code: string, text: string, params: Record<string, string | number>): ChatHistoryTurn {
  return { role: "system", text, ts, code, params };
}

const FINAL = notice(
  300,
  "plan.final_plan",
  "Plano final da spec 001-x, depois da revisão:\n- T01: Servidor\n- T02: Cliente",
  { spec: "001-x" },
);
const INFERRED = notice(200, "plan.consensus_round_inferred", "reserva", { round: 1, issues: 1, accepted: 1, kept: 0, inferred: 1 });
const OPEN = notice(100, "plan.consensus_round_open", "reserva", { round: 1, issues: 1, accepted: 0, kept: 0, open: 1 });

describe("Conversa: avisos do consenso do plano", () => {
  it("a lista final traduz a frase e mantém a lista como veio", () => {
    expect(systemMessageText(FINAL, "en")).toBe("Final plan for spec 001-x, after the review:\n- T01: Servidor\n- T02: Cliente");
    expect(systemMessageText(FINAL, "pt-BR")).toBe(FINAL.text);
    expect(systemMessageText(INFERRED, "en")).toBe(
      "Plan review, round 1: the Reviewer raised 1; the Architect adjusted 1 (1 inferred from the task change, with no written answer) and kept 0.",
    );
  });

  it("rodada sem ponto em aberto e plano final no tom neutro; ponto sem resposta como alerta", () => {
    const id = "plan-notices";
    act(() => appStore.hydrateHistory(id, [OPEN, INFERRED, FINAL]));
    render(<Harness id={id} />);
    const cards = Array.from(document.querySelectorAll<HTMLElement>("[data-system-message]"));
    expect(cards).toHaveLength(3);
    const [open, inferred, final] = cards;
    expect(open?.getAttribute("data-card-tone")).toBe("rose");
    expect(open?.getAttribute("data-system-message")).toBe("");
    for (const card of [inferred, final]) {
      expect(card?.getAttribute("data-card-tone")).toBeNull();
      expect(card?.getAttribute("data-system-message")).toBe("info");
      expect(card?.querySelector(".nd-system-text")?.getAttribute("data-tone")).toBe("info");
    }
    expect(final?.textContent).toContain("- T02: Cliente");
  });
});
