/*
 * Fase de cada projeto no seletor de espaços: a fase exibida é a mesma da pílula
 * (`phaseForProject`), coerente com o estado carregado e com a fase dos eventos.
 */
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState } from "../../api/types";
import { appStore, type ProjectPhase } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask, resetStore } from "../../test-utils";
import { SpaceSwitcher } from "./SpaceSwitcher";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "api-pagamentos");

function withState(specs: ReturnType<typeof makeSpec>[]): ProjectState {
  return makeProjectState({ id: "p1", specs });
}

/** Abre o seletor e devolve a dica de estado do projeto p1. */
function hintOfP1(): string {
  render(<SpaceSwitcher spaceId={null} spaceName="" spacePhase={null} projects={[P1]} attention={null} onSwitch={() => undefined} onNewProject={() => undefined} />);
  fireEvent.click(screen.getByRole("button", { name: /projeto/i }));
  const item = within(screen.getByRole("menu", { name: "Projetos" })).getByRole("menuitemradio", { name: /api-pagamentos/ });
  return item.querySelector(".nd-menu-hint")?.textContent ?? "";
}

describe("seletor de espaços: fase coerente com o estado carregado", () => {
  it("projeto COMPLETED recarregado (sem evento) mostra Concluído", () => {
    resetStore({ projectStates: { p1: withState([makeSpec("s1", "A", "COMPLETED")]) } });
    expect(hintOfP1()).toBe("Concluído");
  });

  it("spec em rascunho mostra Planejando", () => {
    resetStore({ projectStates: { p1: withState([makeSpec("s1", "A", "DRAFT")]) } });
    expect(hintOfP1()).toBe("Planejando");
  });

  it("spec aguardando aprovação mostra Aguardando sua aprovação", () => {
    resetStore({ projectStates: { p1: withState([makeSpec("s1", "A", "AWAITING_APPROVAL")]) } });
    expect(hintOfP1()).toBe("Aguardando sua aprovação");
  });

  it("tarefa BLOCKED mostra Bloqueado", () => {
    resetStore({ projectStates: { p1: withState([makeSpec("s1", "A", "IN_PROGRESS", [makeTask("T01", "t", "BLOCKED")])]) } });
    expect(hintOfP1()).toBe("Bloqueado");
  });

  it("fase armazenada (evento) prevalece sobre o estado carregado", () => {
    resetStore({ phases: { p1: "executing" as ProjectPhase }, projectStates: { p1: withState([makeSpec("s1", "A", "COMPLETED")]) } });
    expect(hintOfP1()).toBe("Executando");
  });

  it("turno do Arquiteto ativo mostra Planejando; ao terminar, Aguardando sua aprovação (spec 081/T03)", () => {
    resetStore({ projectStates: { p1: withState([makeSpec("s1", "A", "AWAITING_APPROVAL")]) } });
    act(() => appStore.beginArchitectTurn("p1", { turnId: "t1", startedAt: Date.now() }));
    expect(hintOfP1()).toBe("Planejando");
    act(() => appStore.endArchitectTurn("p1", "t1"));
    expect(screen.getByRole("menuitemradio", { name: /api-pagamentos/ }).querySelector(".nd-menu-hint")?.textContent).toBe("Aguardando sua aprovação");
  });

  it("projeto sem estado carregado continua ocioso (sem disparar GET)", () => {
    resetStore();
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    expect(hintOfP1()).toBe("Ocioso");
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
