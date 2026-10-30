/*
 * Etapa do plano numa sequência real de eventos (spec 094 T03, decisão 3): o Arquiteto grava o
 * rascunho DURANTE o turno, já como AWAITING_APPROVAL (com a revisão `pending`); o turno termina e
 * a revisão roda (`running`); a revisão conclui (`ok`) e o servidor publica `sdd_draft_updated`.
 * A tela atravessa planning → reviewing → awaiting sem F5 e, em cada etapa, a pílula, os
 * subtítulos, o Plano, a Conversa, o lançador e o sino concordam. O sino segue o `/api/attention`
 * do servidor falso, com o `plan_awaiting_approval` da fixture de paridade.
 */
import { act, cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PlanReviewView, ProjectState } from "../api/types";
import { PLAN_ACTIONS_WAITING_REASON, PLAN_DRAFTING_NOTICE } from "../copy";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../test-utils";
import type { Attention } from "./api";
import { applyAttention } from "./attention";
import { appBadge, renderShell, settle } from "./shell-test-utils";
import { windowSubtitleId } from "./windowMeta";
import type { WindowKind } from "./windows";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  applyAttention(0);
});

const P1 = makeProject("p1", "Loja");
const STATE_GET = /^\/api\/projects\/p1\/state$/;
const APPROVE = "Aprovar e iniciar";
const FIX = "Pedir correção";

/**
 * Estado do projeto com o plano gravado (AWAITING_APPROVAL) e a revisão dada; `turnId` = turno do
 * Arquiteto em andamento (o `/state` do servidor o expõe em `architect_turn`).
 */
function planState(review: PlanReviewView["status"] | null, turnId: string | null = null): ProjectState {
  const spec = {
    ...makeSpec("s1", "Loja online", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO"), makeTask("T02", "Carrinho", "TODO")]),
    scope_warnings: ["T01: critério cita `deps.edn`, fora do escopo"],
    ...(review === null ? {} : { plan_review: { status: review, issues: [] } }),
  };
  const turn = turnId === null ? null : { turn_id: turnId, user_text: "monte o plano", started_at: 1 };
  return makeProjectState({ id: "p1", name: "Loja", specs: [spec], architect_turn: turn });
}

/** `/api/attention` do servidor para o projeto (só o plano entra nesta sequência). */
function attentionWith(planAwaiting: boolean): Attention {
  if (!planAwaiting) return { setup_incomplete: false, projects: [] };
  return {
    setup_incomplete: false,
    projects: [{ project_id: "p1", name: "Loja", pending_permissions: [], pending_choices: false, plan_awaiting_approval: true, paused_or_stalled: null }],
  };
}

function event(type: string, payload: Record<string, unknown>) {
  return { event_type: type, project_id: "p1", payload: { project_id: "p1", ...payload } };
}

const pill = (): string => document.querySelector("[data-run-pill] .nd-run-phase")?.textContent ?? "";
const subtitle = (kind: WindowKind): string => document.getElementById(windowSubtitleId(kind))?.textContent ?? "";
const planWindow = (): HTMLElement | null => screen.queryByRole("region", { name: "Plano" });
const conversation = (): HTMLElement => screen.getByRole("region", { name: "Conversa" });
const bell = (): string => document.querySelector("[data-bell]")?.getAttribute("aria-label") ?? "";
const planReadyCard = (): Element | null => conversation().querySelector("[data-plan-ready]");


/** Item "Aprovar o plano" do lançador (`null` se ausente); fecha o lançador. */
function launcherApprove(): { disabled: string | null } | null {
  fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
  const launcher = screen.getByRole("dialog", { name: "Lançador" });
  fireEvent.change(within(launcher).getByRole("combobox"), { target: { value: "aprovar" } });
  const item = launcher.querySelector<HTMLElement>('[data-launcher-item="approve-plan"]');
  const out = item === null ? null : { disabled: item.getAttribute("aria-disabled") === "true" ? (item.querySelector("[data-launcher-disabled]")?.textContent ?? "") : null };
  fireEvent.keyDown(within(launcher).getByRole("combobox"), { key: "Escape" });
  return out;
}

function button(root: HTMLElement, name: string): HTMLButtonElement | null {
  return within(root).queryByRole("button", { name }) as HTMLButtonElement | null;
}

interface ActionView {
  disabled: boolean;
  title: string;
}

function action(root: HTMLElement, name: string): ActionView | null {
  const el = button(root, name);
  return el === null ? null : { disabled: el.disabled, title: el.title };
}

/**
 * Tudo o que a etapa mostra, num objeto só: a falha nomeia a etapa e o diff aponta o elemento que
 * divergiu (pílula, subtítulo, Plano, Conversa, menu do N, lançador ou sino). `withLauncher` abre e
 * fecha o lançador (efeito colateral), por isso é opcional.
 */
function observe(withLauncher: boolean): Record<string, unknown> {
  const plan = planWindow();
  const card = planReadyCard();
  return {
    pill: pill(),
    conversationSubtitle: subtitle("conversation"),
    planSubtitle: subtitle("plan"),
    planOpen: plan !== null,
    draftingNotice: plan !== null && within(plan).queryAllByText(PLAN_DRAFTING_NOTICE).length > 0,
    reviewerNotice: plan !== null && within(plan).queryByText("O Revisor está revisando o plano…") !== null,
    waitingReason: plan !== null && within(plan).queryAllByText(new RegExp(PLAN_ACTIONS_WAITING_REASON)).length > 0,
    approve: plan === null ? "Plano ausente" : action(plan, APPROVE),
    fix: plan === null ? "Plano ausente" : action(plan, FIX),
    planReadyCard: card === null ? null : (card.textContent ?? "").includes("Plano pronto"),
    nmenuPlanBadge: appBadge("plan"),
    bell: bell(),
    ...(withLauncher ? { launcherApprove: launcherApprove() } : {}),
  };
}

describe("etapa do plano: planning → reviewing → awaiting sem F5 (sequência real)", () => {
  it("pílula, subtítulos, Plano, Conversa, lançador e sino concordam em cada etapa", async () => {
    const { server, stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });

    // 1. O usuário pede o plano: começa o turno; o Arquiteto grava o rascunho DURANTE o turno, já
    //    como AWAITING_APPROVAL (o servidor expõe a revisão `pending` pelo mtime recente).
    await act(async () => {
      stream().emit("chat_turn_started", event("chat_turn_started", { turn_id: "t1", user_text: "monte o plano", started_at: 1 }));
    });
    server.model.states.p1 = planState("pending", "t1");
    server.model.attention = attentionWith(false);
    await act(async () => {
      stream().emit("sdd_draft_updated", event("sdd_draft_updated", {}));
    });
    await settle();

    expect(observe(true), "etapa 1 (planning)").toMatchObject({
      pill: "Planejando",
      conversationSubtitle: "Arquiteto · planejando",
      planSubtitle: "s1 · planejando",
      planOpen: true,
      draftingNotice: true,
      approve: null,
      fix: null,
      reviewerNotice: false,
      planReadyCard: null,
      nmenuPlanBadge: null,
      launcherApprove: null,
      bell: "Precisa de você, nada pendente",
    });

    // 2. Fim do turno: o `chat_turn_ended` recarrega o estado sozinho (sem POST, sem F5); a revisão
    //    do plano já está em curso.
    server.model.states.p1 = planState("running");
    const gets = server.count("GET", STATE_GET);
    await act(async () => {
      stream().emit("chat_turn_ended", event("chat_turn_ended", { turn_id: "t1", ok: true }));
    });
    await settle();
    expect(server.count("GET", STATE_GET), "etapa 2 (reviewing): o fim do turno recarrega o /state").toBeGreaterThan(gets);

    const waiting = { disabled: true, title: PLAN_ACTIONS_WAITING_REASON };
    expect(observe(false), "etapa 2 (reviewing), antes do clique").toMatchObject({
      pill: "Revisando o plano",
      conversationSubtitle: "Arquiteto · revisando o plano",
      planSubtitle: "s1 · revisando o plano",
      planOpen: true,
      approve: waiting,
      fix: waiting,
      waitingReason: true,
    });
    const reviewing = planWindow();
    const fix = reviewing === null ? null : button(reviewing, FIX);
    if (fix !== null) fireEvent.click(fix);
    await settle(3);
    expect(server.count("POST", /\/chat$/), "etapa 2 (reviewing): \"Pedir correção\" desabilitado não envia").toBe(0);
    expect(observe(true), "etapa 2 (reviewing), depois do clique").toMatchObject({
      planReadyCard: null,
      nmenuPlanBadge: null,
      launcherApprove: { disabled: PLAN_ACTIONS_WAITING_REASON },
      bell: "Precisa de você, nada pendente",
    });

    // 3. A revisão conclui: o servidor grava o resultado e publica `sdd_draft_updated`. O relógio de
    //    parede volta 2,3 s neste passo, como o do WSL2 sob carga (causa da falha intermitente, spec
    //    107/T02): o sino não pode ficar esperando o relógio alcançar a consulta anterior.
    server.model.states.p1 = planState("ok");
    server.model.attention = attentionWith(true);
    const wallClock = Date.now();
    const clockBack = vi.spyOn(Date, "now").mockReturnValue(wallClock - 2_300);
    await act(async () => {
      stream().emit("sdd_draft_updated", event("sdd_draft_updated", {}));
    });
    await settle();
    clockBack.mockRestore();

    expect(observe(true), "etapa 3 (awaiting)").toMatchObject({
      pill: "Aguardando sua aprovação",
      conversationSubtitle: "Arquiteto · aguardando sua aprovação",
      planSubtitle: "s1 · aguardando aprovação",
      planOpen: true,
      approve: { disabled: false },
      fix: { disabled: false },
      planReadyCard: true,
      nmenuPlanBadge: "1",
      launcherApprove: { disabled: null },
      bell: "Precisa de você, 1 pendência",
    });
  });

  it("a janela Plano fechada abre sozinha só na TRANSIÇÃO para 'awaiting', sem roubar o foco", async () => {
    const { server, stream } = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: makeProjectState({ id: "p1", name: "Loja" }) } });
    const plan = planWindow();
    if (plan === null) throw new Error("Plano ausente");
    fireEvent.click(within(plan).getByRole("button", { name: "Fechar" }));
    await settle();
    const field = within(conversation()).getByRole("textbox", { name: "Mensagem para o Arquiteto" });
    field.focus();

    // Planejando (rascunho gravado no turno): não abre. Turno "t2": o store guarda os turnos já
    // encerrados (o "t1" do teste anterior) e ignoraria um início repetido.
    await act(async () => {
      stream().emit("chat_turn_started", event("chat_turn_started", { turn_id: "t2", user_text: "monte o plano", started_at: 1 }));
    });
    server.model.states.p1 = planState("pending", "t2");
    await act(async () => {
      stream().emit("sdd_draft_updated", event("sdd_draft_updated", {}));
    });
    await settle();
    expect(pill()).toBe("Planejando");
    expect(planWindow()).toBeNull();

    // Revisando: não abre.
    server.model.states.p1 = planState("running");
    await act(async () => {
      stream().emit("chat_turn_ended", event("chat_turn_ended", { turn_id: "t2", ok: true }));
    });
    await settle();
    expect(pill()).toBe("Revisando o plano");
    expect(planWindow()).toBeNull();

    // Aguardando: abre, e o foco continua no campo da Conversa.
    server.model.states.p1 = planState("stale");
    await act(async () => {
      stream().emit("sdd_draft_updated", event("sdd_draft_updated", {}));
    });
    await settle();
    expect(pill()).toBe("Aguardando sua aprovação");
    expect(planWindow()).not.toBeNull();
    expect(document.activeElement).toBe(field);

    // Fechada de novo, continua fechada enquanto a etapa não muda (só a transição abre).
    const reopened = planWindow();
    if (reopened === null) throw new Error("Plano ausente");
    fireEvent.click(within(reopened).getByRole("button", { name: "Fechar" }));
    await act(async () => {
      stream().emit("sdd_draft_updated", event("sdd_draft_updated", {}));
    });
    await settle();
    expect(planWindow()).toBeNull();
  });
});
