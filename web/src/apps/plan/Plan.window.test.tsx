/*
 * Janela Plano montada pelo shell (spec 093, T06): bloqueio do "Aprovar e iniciar" por
 * `readiness.execute`, o 412 `prereq_missing`, o estado vazio, o pedido de correção enviado pela
 * Conversa, o subtítulo vivo e os ícones.
 */
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ProjectState, SpecView } from "../../api/types";
import { findForbiddenGlyphs } from "../../desktop/glyphs";
import { notReady, renderShell, settle } from "../../desktop/shell-test-utils";
import { windowSubtitleId } from "../../desktop/windowMeta";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";
import { planSummary } from "./summary";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const P1 = makeProject("p1", "Loja");
const APPROVE = /\/api\/projects\/p1\/specs\/s1\/approve$/;
const CHAT = /^\/api\/projects\/p1\/chat$/;

function spec(extra: Partial<SpecView> = {}): SpecView {
  return { ...makeSpec("s1", "Loja online", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Carrinho", "TODO", { depends_on: ["T01"] })]), ...extra };
}

function state(s: SpecView | null): ProjectState {
  return makeProjectState({ id: "p1", name: "Loja", specs: s === null ? [] : [s] });
}

function plan(): HTMLElement {
  return screen.getByRole("region", { name: "Plano" });
}

describe("Plano: aprovação e prontidão", () => {
  it("sem readiness.execute: Aprovar desabilitado com o que falta e Configurar na aba Máquina", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(spec()) }, readiness: notReady() });
    const approve = within(plan()).getByRole("button", { name: "Aprovar e iniciar" }) as HTMLButtonElement;
    expect(approve.disabled).toBe(true);
    const notice = within(plan()).getByRole("region", { name: "Para aprovar e iniciar a execução, falta:" });
    expect(notice.textContent).toContain("Docker daemon");
    const docker = within(notice).getByRole("button", { name: "Configurar: Docker daemon" });
    expect(docker.getAttribute("data-settings-tab")).toBe("machine");
    fireEvent.click(docker);
    await settle();
    expect(screen.getByRole("region", { name: "Configurações" })).toBeTruthy();
  });

  it("aprovar pede confirmação e chama approve; o 412 mostra o que falta e relê a prontidão", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(spec()) } });
    server.reply("POST", APPROVE, () => ({ status: 412, body: { error: "prereq_missing", missing: ["docker_daemon", "agent_reviewer"] } }));
    const before = server.count("GET", /^\/api\/readiness$/);
    fireEvent.click(within(plan()).getByRole("button", { name: "Aprovar e iniciar" }));
    expect(server.count("POST", APPROVE)).toBe(0);
    await act(async () => {
      fireEvent.click(within(plan()).getByRole("button", { name: "Confirmar início" }));
    });
    await settle();
    expect(server.count("POST", APPROVE)).toBe(1);
    const alert = within(plan()).getByRole("alert");
    expect(alert.textContent).toContain("Docker daemon");
    expect(within(alert).getByRole("button", { name: "Configurar: Revisor" }).getAttribute("data-settings-tab")).toBe("agents");
    expect(server.count("GET", /^\/api\/readiness$/)).toBeGreaterThan(before);
  });

  it("aprovado (202): relê o estado e o /api/attention", async () => {
    const { server } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(spec()) } });
    server.reply("POST", APPROVE, () => {
      server.model.states.p1 = state(spec({ status: "IN_PROGRESS" }));
      return { status: 202, body: { ok: true, spec_id: "s1" } };
    });
    const attentionBefore = server.count("GET", /^\/api\/attention$/);
    fireEvent.click(within(plan()).getByRole("button", { name: "Aprovar e iniciar" }));
    await act(async () => {
      fireEvent.click(within(plan()).getByRole("button", { name: "Confirmar início" }));
    });
    await settle();
    // Em execução, o layout troca de categoria (o Plano sai) e o botão some.
    await waitFor(() => expect(screen.queryByRole("button", { name: "Aprovar e iniciar" })).toBeNull());
    expect(server.count("GET", /\/api\/projects\/p1\/state$/)).toBeGreaterThan(1);
    expect(server.count("GET", /^\/api\/attention$/)).toBeGreaterThan(attentionBefore);
  });
});

describe("Plano: conteúdo, vazio e correção", () => {
  it("sem plano: estado vazio com 'Abrir a Conversa'", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(null) } });
    const win = plan();
    expect(win.textContent).toContain("O Arquiteto ainda não montou o plano.");
    fireEvent.click(within(win).getByRole("button", { name: "Abrir a Conversa" }));
    await settle();
    expect(screen.getByRole("region", { name: "Conversa" })).toBeTruthy();
  });

  it("tarefas com dependência, subtítulo vivo e nenhum caractere no lugar de ícone", async () => {
    await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: state(spec({ plan_changes: ["T02 nova"] })) } });
    const win = plan();
    expect(within(win).getByRole("list", { name: "Tarefas do plano" }).textContent).toContain("depende de T01");
    // A-09 (094/T08): só o id identifica a tarefa (sem "01" numa caixa) e a dependência usa a linha de metadados.
    const list = within(win).getByRole("list", { name: "Tarefas do plano" });
    expect(list.textContent).not.toMatch(/\b0\d\b/);
    const meta = list.querySelector("[data-task-meta]");
    expect(meta?.classList.contains("nd-meta-line")).toBe(true);
    expect(meta?.querySelector(".nd-mono")).toBeNull();
    expect(document.getElementById(windowSubtitleId("plan"))?.textContent).toBe("s1 · aguardando aprovação");
    expect(findForbiddenGlyphs(win)).toEqual([]);
  });

  it("'Pedir correção ao Arquiteto' vai pela Conversa (mensagem do usuário e POST do chat)", async () => {
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [P1],
      states: { p1: state(spec({ scope_warnings: ["T01: cita deps.edn"] })) },
    });
    server.reply("POST", CHAT, () => ({ status: 200, body: { reply: "Corrigido.", draft_updated: false, turn_id: "t7" } }));
    await act(async () => {
      fireEvent.click(within(plan()).getByRole("button", { name: "Pedir correção" }));
    });
    await settle();
    expect(server.count("POST", CHAT)).toBe(1);
    const body = server.calls.find((c) => c.method === "POST" && CHAT.test(c.url))?.body as { message: string };
    expect(body.message).toContain("Corrija estes pontos das checagens do plano:");
    expect(body.message).toContain("- T01: cita deps.edn");
    const conversation = screen.getByRole("region", { name: "Conversa" });
    expect(within(conversation).getByText("Corrigido.")).toBeTruthy();
  });
});

describe("Plano: resumo (função pura)", () => {
  const base = {
    readiness: null,
    projectCount: 1,
  };
  it("selo de plano pronto só quando pode aprovar; subtítulo pela etapa (planStage)", () => {
    const ready = spec();
    const data = { activeSpec: ready, planReady: ready, planStage: "awaiting" } as never;
    expect(planSummary({ ...base, space: data })).toEqual({ subtitle: "s1 · aguardando aprovação", badge: { count: 1, label: "Plano pronto para aprovar" } });
    const drafting = { activeSpec: ready, planReady: null, planStage: "planning" } as never;
    expect(planSummary({ ...base, space: drafting }).badge).toBeUndefined();
    expect(planSummary({ ...base, space: drafting }).subtitle).toBe("s1 · planejando");
    const reviewing = spec({ plan_review: { status: "running", issues: [] } });
    const review = planSummary({ ...base, space: { activeSpec: reviewing, planReady: null, planStage: "reviewing" } as never });
    expect(review).toEqual({ subtitle: "s1 · revisando o plano" });
    expect(planSummary({ ...base, space: { activeSpec: null, planReady: null, planStage: null } as never })).toMatchObject({ subtitle: "sem plano" });
  });
});
