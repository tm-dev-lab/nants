/*
 * Conversa enxuta (095/T04, decisão 7; `reference/Main.dc.html`):
 * - separador de dia ("Hoje", "Ontem", data) entre linhas, fora da lista de mensagens;
 * - "Você · hh:mm" acima da bolha, ou só "Você" sem hora (C-16);
 * - Arquiteto, agentes, Revisor, sistema, permissão, pausa e relatório no mesmo cartão
 *   (`nd-chat-card`), com cabeçalho (avatar, nome, hora) e corpo;
 * - rodapé "Plano pronto · N tarefas · revisado · Abrir" na última mensagem do Arquiteto;
 * - atividade dos agentes e aprovação do YOLO como pílula central entre linhas;
 * - composer com Enviar quadrado (a primária só com texto).
 */
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SpecView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import type { ActivityEntry, ChatMessage, PendingPermission } from "../../store";
import { makeSpec, makeTask, resetStore } from "../../test-utils";
import { Conversation } from "./Conversation";
import { clockLabel } from "./chatTime";

vi.mock("../../projectState", () => ({ refreshProjectState: vi.fn(async () => null) }));

afterEach(cleanup);

const NOW = Date.now();
const YESTERDAY = NOW - 24 * 60 * 60 * 1000;
const SPEC: SpecView = makeSpec("s1", "Sistema Solar", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Cena", "IN_PROGRESS")]);

function view(messages: readonly ChatMessage[], extra: Partial<Parameters<typeof Conversation>[0]> = {}) {
  resetStore({});
  return render(<Conversation projectId="p1" spec={SPEC} messages={messages} onDraftUpdated={async () => {}} {...extra} />);
}

const list = (): HTMLElement => screen.getByRole("list", { name: "Mensagens da conversa" });

describe("Conversa enxuta: dia e hora", () => {
  it("separador de dia dentro do item que abre cada dia; a lista só tem listitem como filho (ARIA 1.2)", () => {
    view([
      { id: 1, role: "user", text: "ontem", at: YESTERDAY },
      { id: 2, role: "architect", agent: "architect", text: "resposta de ontem", at: YESTERDAY + 1000 },
      { id: 3, role: "user", text: "hoje", at: NOW },
    ]);
    const separators = within(list()).getAllByRole("separator");
    expect(separators.map((s) => s.getAttribute("aria-label"))).toEqual(["Ontem", "Hoje"]);
    expect(separators.every((s) => s.classList.contains("nd-day-sep"))).toBe(true);
    // O separador fica dentro do item, antes da mensagem que abre o dia.
    expect(separators.map((s) => s.parentElement?.tagName)).toEqual(["LI", "LI"]);
    expect(separators[0]?.nextElementSibling?.textContent).toContain("ontem");
    expect(separators[1]?.parentElement?.textContent).toContain("hoje");
    // Todo filho direto da lista é <li> sem role; as mensagens continuam como os únicos itens.
    const children = Array.from(list().children);
    expect(children.every((c) => c.tagName === "LI" && !c.hasAttribute("role"))).toBe(true);
    expect(within(list()).getAllByRole("listitem")).toHaveLength(3);
  });

  it("'Você · hh:mm' acima da bolha; sem hora, só 'Você' (C-16) e sem separador", () => {
    view([
      { id: 1, role: "user", text: "com hora", at: NOW },
      { id: 2, role: "user", text: "sem hora", at: 0 },
    ]);
    const metas = Array.from(document.querySelectorAll("[data-user-meta]")).map((m) => m.textContent);
    // A linha sem hora vem antes (ordem por instante) e não abre separador de dia.
    expect(metas).toEqual(["Você", `Você · ${clockLabel(NOW)}`]);
    const first = screen.getByText("com hora");
    expect(first.previousElementSibling?.hasAttribute("data-user-meta")).toBe(true);
    expect(within(list()).getAllByRole("separator")).toHaveLength(1);
  });
});

describe("Conversa enxuta: cartões", () => {
  it("Arquiteto em cartão com cabeçalho (avatar, nome, hora) e corpo", () => {
    view([{ id: 1, role: "architect", agent: "architect", text: "Montei o plano em 2 tarefas.", at: NOW }]);
    const card = screen.getByText("Montei o plano em 2 tarefas.").closest(".nd-chat-card");
    if (card === null) throw new Error("cartão ausente");
    const head = card.querySelector(".nd-chat-card-head");
    expect(head?.querySelector('[data-kind="architect"]')).not.toBeNull();
    expect(head?.textContent).toContain("Arquiteto");
    expect(head?.querySelector("time")?.textContent).toBe(clockLabel(NOW));
    expect(card.querySelector(".nd-chat-card-body")?.textContent).toContain("Montei o plano");
  });

  it("Revisor, outros agentes e sistema usam o mesmo cartão; o agrupado não repete o cabeçalho", () => {
    view([
      { id: 1, role: "agent", agent: "developer", text: "implementei", at: NOW, taskId: "T02", specId: "s1" },
      { id: 2, role: "agent", agent: "developer", text: "testes ok", at: NOW + 1, taskId: "T02", specId: "s1" },
      { id: 3, role: "agent", agent: "reviewer", text: "Veredito: APPROVED", at: NOW + 2, taskId: "T02", specId: "s1" },
      { id: 4, role: "system", text: "Falha ao falar com o provedor", at: NOW + 3 },
    ]);
    for (const text of ["implementei", "testes ok", "Falha ao falar com o provedor"]) {
      expect(screen.getByText(text).closest(".nd-chat-card")).not.toBeNull();
    }
    expect(document.querySelector('[data-agent-card="reviewer"]')).not.toBeNull();
    const grouped = screen.getByText("testes ok").closest(".nd-chat-card");
    expect(grouped?.getAttribute("data-grouped")).toBe("true");
    expect(grouped?.querySelector(".nd-chat-card-head")).toBeNull();
    expect(screen.getByText("Falha ao falar com o provedor").closest(".nd-chat-card")?.getAttribute("data-card-tone")).toBe("rose");
  });

  it("permissão pendente: cartão âmbar com o cabeçalho do agente", () => {
    const request: PendingPermission = { requestId: "r1", projectId: "p1", agent: "developer", taskId: "T02", command: "npm install", at: NOW };
    view([], { pending: [request] });
    const card = document.querySelector("[data-permission-card]");
    expect(card?.classList.contains("nd-chat-card")).toBe(true);
    expect(card?.getAttribute("data-card-tone")).toBe("amber");
    expect(card?.querySelector(".nd-chat-card-head")?.textContent).toContain("Desenvolvedor");
  });
});

describe("Conversa enxuta: plano pronto no rodapé do Arquiteto", () => {
  const ready: SpecView = { ...makeSpec("s1", "Sistema Solar", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Cena", "TODO")]), plan_review: { status: "ok", issues: [] } };

  it("na última mensagem do Arquiteto: 'Plano pronto · 2 tarefas · revisado · Abrir'", () => {
    view(
      [
        { id: 1, role: "architect", agent: "architect", text: "primeira", at: NOW },
        { id: 2, role: "architect", agent: "architect", text: "Montei o plano.", at: NOW + 1 },
      ],
      { spec: ready, planReady: ready },
    );
    const footer = document.querySelector<HTMLElement>("[data-plan-ready]");
    if (footer === null) throw new Error("rodapé ausente");
    expect(footer.classList.contains("nd-chat-card-foot")).toBe(true);
    expect(footer.closest(".nd-chat-card")?.textContent).toContain("Montei o plano.");
    expect(footer.textContent).toBe("Plano pronto2 tarefas · revisadoAbrir");
    expect(within(footer).getByRole("button", { name: "Abrir Plano" })).toBeTruthy();
    expect(document.querySelectorAll("[data-plan-ready]")).toHaveLength(1);
    expect(findForbiddenGlyphs(footer)).toEqual([]);
  });

  it("apontamentos do Revisor no lugar de 'revisado'; sem mensagem do Arquiteto, um cartão próprio", () => {
    const withIssues: SpecView = { ...ready, plan_review: { status: "ok", issues: [{ task: "T02", kind: "untestable", text: "x" }] } };
    view([], { spec: withIssues, planReady: withIssues });
    const footer = document.querySelector<HTMLElement>("[data-plan-ready]");
    expect(footer?.textContent).toContain("2 tarefas · 1 apontamento do Revisor");
    expect(footer?.parentElement?.classList.contains("nd-chat-card")).toBe(true);
  });
});

describe("Conversa enxuta: eventos dos agentes e composer", () => {
  it("atividade ao vivo como pílula central entre linhas", () => {
    const activity: ActivityEntry = { key: "d1", agent: "developer", taskId: "T02", activityId: "a1", reasoning: "", text: "", tool: "gerando textura de Júpiter", startedAt: NOW, lastAt: NOW };
    view([], { activities: [activity] });
    const pill = document.querySelector(".nd-event-line .nd-event-pill");
    expect(pill?.textContent).toBe("Desenvolvedor · T02 · gerando textura de Júpiter");
  });

  it("aprovação automática do YOLO também é pílula de evento", () => {
    view([{ id: 1, role: "agent", agent: "developer", text: "aprovado automaticamente (YOLO): npm ci", at: NOW, specId: "s1" }]);
    const event = screen.getByTestId("yolo-auto");
    expect(event.classList.contains("nd-event-line")).toBe(true);
    expect(event.querySelector(".nd-event-pill code")?.textContent).toBe("npm ci");
  });

  it("composer com Enviar quadrado: primária só com texto", () => {
    view([]);
    const send = screen.getByRole("button", { name: "Enviar" });
    expect(send.classList.contains("nd-composer-send")).toBe(true);
    expect(send.getAttribute("data-variant")).toBeNull();
    expect(send.closest(".nd-composer-dock")).not.toBeNull();
  });
});
