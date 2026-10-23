/*
 * Execução pausada ou parada na Conversa nos dois idiomas (spec 102, T02): título, explicação, a
 * análise escalada (rótulos do bloco "Contexto"), as opções com confirmação, a orientação livre e
 * as falhas, sem português em en. O conteúdo do Arquiteto e das opções aparece como foi escrito.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { parseApiEvent } from "../../api/events";
import type { ProjectState, RecoveryOption, SpecView } from "../../api/types";
import { notReady, READY, renderShell, settle } from "../../desktop/shell-test-utils";
import { setLanguage, type Lang } from "../../i18n";
import { portugueseLeftovers } from "../../i18n/ptLeftovers";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  setLanguage("pt-BR", { persist: false });
});

const PID = "p1";
const RESUME = /\/api\/projects\/p1\/specs\/s1\/resume$/;
const CHOICE = /\/api\/projects\/p1\/specs\/s1\/tasks\/T01\/recovery-choice$/;

const OPTIONS: RecoveryOption[] = [
  { id: "o1", label: "Use npx tsc", kind: "config", test_command: "npx tsc", diff: "tsc -> npx tsc" },
  { id: "o2", label: "Install curl", kind: "environment", files: [{ path: ".nants/Dockerfile", content: "FROM x" }], text: "Add curl" },
];

/** Análise do Arquiteto com o bloco "Contexto" do servidor (marcas em pt-BR, valores em inglês). */
const ANALYSIS =
  "Análise de recuperação #1\nCausa raiz: The port is busy\n- Contexto:\n  - Comando: npm test\n  - Código: 1\n  - Trecho do erro: EADDRINUSE\n  - O que foi tentado: restart\n  - Por que não resolveu: still busy\n  - Opções: (1) free the port; (2) use another";

function pausedSpec(): SpecView {
  return makeSpec("s1", "Shop", "IN_PROGRESS", [
    makeTask("T01", "Checkout", "BLOCKED", { attempts: 3, next_action: "port busy", recovery_options: OPTIONS, recovery_options_analysis: 7 } as never),
    makeTask("T02", "Cart", "BLOCKED", { next_action: "waiting" }),
  ]);
}

function stateWith(spec: SpecView): ProjectState {
  return makeProjectState({ id: PID, name: "Shop", specs: [spec] });
}

async function open(lang: Lang, state: ProjectState, readiness = READY) {
  setLanguage(lang, { persist: false });
  const shell = await renderShell({ url: "/?space=p1", projects: [makeProject(PID, "Shop")], states: { p1: state }, readiness });
  await settle();
  return shell;
}

function applyEvent(type: string, payload: Record<string, unknown>): void {
  const event = parseApiEvent({ event_type: type, project_id: PID, payload });
  if (event === null) throw new Error("evento inválido no teste");
  act(() => appStore.applyEvent(event));
}

function card(): HTMLElement {
  const el = document.querySelector<HTMLElement>("[data-paused-card]");
  if (el === null) throw new Error("cartão ausente");
  return el;
}

describe("Execução pausada em en", () => {
  it("cartão completo sem português: análise, opções, confirmação e orientação", async () => {
    await open("en", stateWith(pausedSpec()));
    applyEvent("agent_message", { agent: "architect", text: ANALYSIS, task_id: "T01", spec_id: "s1", seq: 1 });
    await settle();
    const article = card();
    expect(within(article).getByText("Run paused")).toBeTruthy();
    expect(within(article).getByText("The Architect analyzed it and escalated to you")).toBeTruthy();
    expect(within(article).getByText("What happened")).toBeTruthy();
    expect(within(article).getByText("Exit code: 1")).toBeTruthy();
    expect(within(article).getByText("Adjust verification")).toBeTruthy();
    expect(within(article).getByText((_, el) => el?.classList.contains("nd-error-inline") === true && el.textContent === "T01 blocked")).toBeTruthy();
    fireEvent.click(within(article).getByRole("button", { name: /See proposed files/ }));
    fireEvent.click(within(article).getByRole("button", { name: "Choose: Install curl" }));
    expect(within(article).getByRole("button", { name: "Apply and resume" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Cancel" })).toBeTruthy();
    fireEvent.click(within(article).getByRole("button", { name: "I prefer another path" }));
    expect(within(article).getByRole("textbox", { name: "Other guidance" })).toBeTruthy();
    expect(within(article).getByRole("button", { name: "Resume" }).getAttribute("data-resume")).toBe("");
    for (const details of article.querySelectorAll("details")) details.open = true;
    expect(portugueseLeftovers(article)).toEqual([]);
  });

  it("causas fixas do servidor e a contagem no idioma ativo (spec 115 T04)", async () => {
    const limit =
      "Análise de recuperação #3 (ESCALATE)\n- Causa raiz: limite de ciclos de recuperação atingido\n- Decisão: escalar ao humano\n- Contexto:\n  - comando: npm test\n  - código: 127\n  - o que foi tentado: 2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas\n  - por que não resolveu: limite de ciclos de recuperação atingido";
    await open("en", stateWith(pausedSpec()));
    applyEvent("agent_message", { agent: "architect", text: limit, task_id: "T01", spec_id: "s1", seq: 1 });
    await settle();
    const article = card();
    expect(within(article).getAllByText("recovery cycle limit reached")).toHaveLength(2);
    expect(within(article).getByText("2 of 2 recovery cycles and 0 of 2 autonomous rounds")).toBeTruthy();
    for (const details of article.querySelectorAll("details")) details.open = true;
    expect(portugueseLeftovers(article)).toEqual([]);
    cleanup();
    await open("pt-BR", stateWith(pausedSpec()));
    applyEvent("agent_message", { agent: "architect", text: limit, task_id: "T01", spec_id: "s1", seq: 1 });
    await settle();
    expect(within(card()).getAllByText("limite de ciclos de recuperação atingido")).toHaveLength(2);
    expect(within(card()).getByText("2 de 2 ciclos de recuperação e 0 de 2 rodadas autônomas")).toBeTruthy();
  });

  it("sem prontidão: o que falta e Configurar em en", async () => {
    await open("en", stateWith(pausedSpec()), notReady());
    const article = card();
    expect(within(article).getByText("To resume the run, you still need:")).toBeTruthy();
    expect(portugueseLeftovers(article)).toEqual([]);
  });

  it("falhas da escolha e da retomada em en", async () => {
    const { server } = await open("en", stateWith(pausedSpec()));
    server.reply("POST", CHOICE, () => ({ status: 409, body: { error: "x", reason: "budget_exceeded" } }));
    fireEvent.click(within(card()).getByRole("button", { name: "Choose: Use npx tsc" }));
    await act(async () => {
      fireEvent.click(within(card()).getByRole("button", { name: "Apply and resume" }));
    });
    await settle();
    expect(within(card()).getByRole("alert").textContent).toBe("The cost budget was reached. Nothing was applied.");
    server.reply("POST", RESUME, () => ({ status: 412, body: { error: "prereq_missing", missing: ["docker_daemon"] } }));
    await act(async () => {
      fireEvent.click(within(card()).getByRole("button", { name: "Resume" }));
    });
    await settle();
    const alert = within(card()).getByRole("alert");
    expect(alert.textContent).toContain("Still to set up:");
    expect(within(alert).getByRole("button", { name: /^Set up: / })).toBeTruthy();
    expect(portugueseLeftovers(card())).toEqual([]);
  });

  it("parada sem bloqueio: Continuar em en; pt-BR igual a hoje", async () => {
    const stalled = makeSpec("s1", "Shop", "IN_PROGRESS", [makeTask("T01", "A", "IN_PROGRESS")]);
    await open("en", stateWith(stalled));
    expect(within(card()).getByText("Run stopped")).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Continue" })).toBeTruthy();
    cleanup();
    await open("pt-BR", stateWith(stalled));
    expect(within(card()).getByText("Execução parada")).toBeTruthy();
    expect(within(card()).getByRole("button", { name: "Continuar" })).toBeTruthy();
    expect(screen.getByRole("list", { name: "Mensagens da conversa" })).toBeTruthy();
  });
});
