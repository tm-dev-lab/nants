/* Bolhas de atividade (spec 093, T06; compartilhadas com a janela Atividade). */
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ActivityBubbles, ToolLabel } from "./ActivityBubble";
import { Conversation } from "../conversation/Conversation";
import { useActivityPrune, useSpaceActivities } from "../../desktop/useSpaceData";
import { appStore } from "../../store";
import type { ApiEvent } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";

/** Conversa com as atividades por seletor e a poda do shell (`useActivityPrune`). */
function Harness() {
  useActivityPrune("p1");
  const activities = useSpaceActivities("p1");
  return <Conversation projectId="p1" messages={[]} activities={activities} onDraftUpdated={async () => undefined} />;
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  appStore.setState({ activities: {} });
});

describe("ActivityBubbles", () => {
  it("mostra o rótulo da etapa no cabeçalho", () => {
    render(<ActivityBubbles activities={[{ key: "k", agent: "architect", reasoning: "", tool: "", text: "", stage: "Escrevendo relatório final" }]} />);
    expect(screen.getByTestId("activity-stage").textContent).toBe("Escrevendo relatório final");
  });

  it("mostra papel, pensando, reasoning, ferramenta e rascunho; resumo discreto", () => {
    render(
      <ActivityBubbles
        activities={[{ key: "k", agent: "developer", taskId: "T01", reasoning: "analisando", tool: "lendo src/x.rs", text: "rascunho" }]}
      />,
    );
    expect(screen.getByText("Desenvolvedor")).not.toBeNull();
    expect(screen.getAllByText(/pensando/).length).toBeGreaterThan(0);
    expect(screen.getByTestId("activity-reasoning").className).toContain("nd-activity-reasoning");
    expect(screen.getByTestId("activity-tool").textContent).toBe("lendo src/x.rs");
    expect(screen.getByTestId("activity-text").textContent).toBe("rascunho");
    expect(screen.getByTestId("activity-summary").getAttribute("aria-live")).toBe("polite");
    expect(screen.getByTestId("activity-summary").textContent).toBe("Desenvolvedor pensando");
  });

  it("comando em curso: executando `cmd` com relógio m:ss", () => {
    vi.useFakeTimers();
    vi.setSystemTime(10_000_000);
    render(
      <ActivityBubbles
        activities={[{ key: "k", agent: "developer", reasoning: "", tool: "", text: "", command: "npm test", commandStartedAt: 10_000_000 - 65_000 }]}
      />,
    );
    expect(screen.getByTestId("activity-command").textContent).toBe("executando `npm test` · 1:05");
    act(() => {
      vi.advanceTimersByTime(3000);
    });
    expect(screen.getByTestId("activity-command").textContent).toBe("executando `npm test` · 1:08");
  });

  it("o comando e o estado da pílula são texto e ponto (sem caractere proibido)", () => {
    const { container } = render(
      <ActivityBubbles activities={[{ key: "k", agent: "tests", reasoning: "", tool: "", text: "", command: "npm test", commandStartedAt: Date.now() }]} />,
    );
    // 095/T04: o evento é uma pílula central entre linhas; o estado (trabalhando) fica na pílula.
    const pill = container.querySelector(".nd-event-line .nd-event-pill[data-state='working']");
    expect(pill).not.toBeNull();
    expect(pill?.querySelector(".nd-dot[data-tone='green']")).not.toBeNull();
    expect(pill?.textContent).toBe("Testes · rodando...");
    expect(findForbiddenGlyphs(container)).toEqual([]);
  });

  it("sem atividades não renderiza bolhas", () => {
    render(<ActivityBubbles activities={[]} />);
    expect(screen.queryByTestId("activity-bubble")).toBeNull();
  });
});

function ev(payload: Record<string, unknown>): ApiEvent {
  return { event_type: "agent_activity", project_id: "p1", payload } as unknown as ApiEvent;
}

describe("Conversa: bolha de atividade", () => {
  it("aparece com eventos, persiste entre passos, some após a carência e com a mensagem final", () => {
    vi.useFakeTimers();
    render(<Harness />);
    expect(screen.queryByTestId("activity-bubble")).toBeNull();
    act(() => {
      appStore.applyEvent(ev({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "reasoning", text: "pensando fundo" }));
    });
    expect(screen.getByTestId("activity-reasoning").textContent).toBe("pensando fundo");
    act(() => {
      appStore.applyEvent(ev({ agent: "developer", task_id: "T01", activity_id: "a1", kind: "end" }));
      vi.advanceTimersByTime(1_000);
    });
    expect(screen.getByTestId("activity-bubble")).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(1_000);
    });
    expect(screen.queryByTestId("activity-bubble")).toBeNull();

    act(() => {
      appStore.applyEvent(ev({ agent: "developer", task_id: "T01", activity_id: "a2", kind: "start" }));
    });
    expect(screen.getByTestId("activity-bubble")).not.toBeNull();
    act(() => {
      appStore.applyEvent({
        event_type: "agent_message",
        project_id: "p1",
        payload: { agent: "developer", text: "feito", task_id: "T01" },
      } as ApiEvent);
    });
    expect(screen.queryByTestId("activity-bubble")).toBeNull();
  });

  it("inatividade longa remove a bolha", () => {
    vi.useFakeTimers();
    render(<Harness />);
    act(() => {
      appStore.applyEvent(ev({ agent: "reviewer", activity_id: "r1", kind: "start" }));
    });
    expect(screen.getByTestId("activity-bubble")).not.toBeNull();
    act(() => {
      vi.advanceTimersByTime(121_000);
    });
    expect(screen.queryByTestId("activity-bubble")).toBeNull();
  });
});

describe("ToolLabel", () => {
  it("URL http/https vira link seguro (nova aba, noopener noreferrer); texto hostil segue texto", () => {
    const { container } = render(<ToolLabel text={"fonte consultada: <img src=x onerror=alert(1)> (https://exemplo.com/a?b=1)"} />);
    const a = container.querySelector("a");
    expect(a?.getAttribute("href")).toBe("https://exemplo.com/a?b=1");
    expect(a?.getAttribute("target")).toBe("_blank");
    expect(a?.getAttribute("rel")).toBe("noopener noreferrer");
    expect(container.querySelector("img")).toBeNull();
    expect(container.textContent).toContain("<img src=x onerror=alert(1)>");
  });

  it("javascript: nunca vira link", () => {
    const { container } = render(<ToolLabel text="lendo página javascript:alert(1)" />);
    expect(container.querySelector("a")).toBeNull();
    expect(container.textContent).toBe("lendo página javascript:alert(1)");
  });

  it("lendo página http renderiza link; outros esquemas ficam texto", () => {
    const { container } = render(<ToolLabel text="lendo página http://a.dev/x e ftp://b.dev/y" />);
    const links = container.querySelectorAll("a");
    expect(links).toHaveLength(1);
    expect(links[0]?.getAttribute("href")).toBe("http://a.dev/x");
    expect(container.textContent).toBe("lendo página http://a.dev/x e ftp://b.dev/y");
  });
});
