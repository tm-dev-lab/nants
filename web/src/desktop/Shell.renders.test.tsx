/*
 * Desempenho do shell (revisão da T05, F-01): o streaming de atividades e os eventos de outros
 * projetos não re-renderizam a barra superior, as molduras das janelas nem quem usa `useSpace()`.
 * Só re-renderiza quem lê as atividades pelo seletor próprio (`useSpaceActivities`).
 */
import { act, cleanup } from "@testing-library/react";
import { createElement, memo, Profiler, type ComponentProps } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "../api/types";
import { appStore } from "../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../test-utils";
import { renderShell, settle } from "./shell-test-utils";
import { manualScheduler } from "./test-utils";
import { useSpace } from "./shellContext";
import { useSpaceActivities } from "./useSpaceData";

const renders = vi.hoisted(() => ({ topBar: 0, frames: {} as Record<string, number>, spaceReader: 0, activityReader: 0, conversation: 0, plan: 0, board: 0, activity: 0 }));

vi.mock("../apps/conversation/Conversation", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../apps/conversation/Conversation")>();
  // A Conversa real (T06), contada a cada render.
  function CountedConversation(props: ComponentProps<typeof mod.Conversation>) {
    renders.conversation += 1;
    return createElement(mod.Conversation, props);
  }
  return { ...mod, Conversation: CountedConversation };
});

vi.mock("../apps/plan/PlanCard", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../apps/plan/PlanCard")>();
  function CountedPlanCard(props: ComponentProps<typeof mod.PlanCard>) {
    renders.plan += 1;
    return createElement(mod.PlanCard, props);
  }
  return { ...mod, PlanCard: CountedPlanCard };
});

// Quadro e Atividade reais (T07): o Profiler conta cada commit da árvore deles, inclusive os
// re-renders vindos do store ou do contexto (não só os do pai).
vi.mock("../apps/board/BoardApp", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../apps/board/BoardApp")>();
  const Counted = memo(function CountedBoard(props: ComponentProps<typeof mod.BoardContent>) {
    return createElement(Profiler, { id: "board", onRender: () => (renders.board += 1) }, createElement(mod.BoardContent, props));
  });
  return { ...mod, BoardContent: Counted };
});

vi.mock("../apps/activity/ActivityApp", async (importOriginal) => {
  const mod = await importOriginal<typeof import("../apps/activity/ActivityApp")>();
  const Counted = memo(function CountedActivity(props: ComponentProps<typeof mod.ActivityContent>) {
    return createElement(Profiler, { id: "activity", onRender: () => (renders.activity += 1) }, createElement(mod.ActivityContent, props));
  });
  return { ...mod, ActivityContent: Counted };
});

vi.mock("./topbar/TopBar", async (importOriginal) => {
  const mod = await importOriginal<typeof import("./topbar/TopBar")>();
  // Mesma semântica (memo raso) da barra real; conta cada render efetivo.
  const Counted = memo(function CountedTopBar(props: ComponentProps<typeof mod.TopBar>) {
    renders.topBar += 1;
    return createElement(mod.TopBar, props);
  });
  return { ...mod, TopBar: Counted };
});

vi.mock("./Window", async (importOriginal) => {
  const mod = await importOriginal<typeof import("./Window")>();
  const Counted = memo(function CountedWindow(props: ComponentProps<typeof mod.Window>) {
    renders.frames[props.kind] = (renders.frames[props.kind] ?? 0) + 1;
    return createElement(mod.Window, props);
  });
  return { ...mod, Window: Counted };
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  appStore.setState({ activities: {} });
});

function SpaceReader() {
  renders.spaceReader += 1;
  const space = useSpace();
  return createElement("p", null, space?.data.name ?? "");
}

function ActivityReader() {
  renders.activityReader += 1;
  const space = useSpace();
  const activities = useSpaceActivities(space?.data.projectId ?? "");
  return createElement("p", { "data-activity-count": "" }, String(activities.length));
}

function activity(projectId: string, i: number): ApiEvent {
  return {
    event_type: "agent_activity",
    project_id: projectId,
    payload: { agent: "developer", activity_id: "a1", kind: "reasoning", text: `passo ${i}` },
  } as unknown as ApiEvent;
}

function reset(): void {
  renders.topBar = 0;
  renders.frames = {};
  renders.spaceReader = 0;
  renders.activityReader = 0;
  renders.conversation = 0;
  renders.plan = 0;
  renders.board = 0;
  renders.activity = 0;
}

describe("Shell: re-renderização", () => {
  it("rajada de agent_activity: só o leitor de atividades re-renderiza; barra e molduras não", async () => {
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja"), makeProject("p2", "Blog")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
      apps: { conversation: { Content: ActivityReader, scroll: "app" }, plan: { Content: SpaceReader, scroll: "app" } },
    });
    // Primeira atividade cria a entrada; a rajada seguinte só atualiza o texto.
    await act(async () => {
      stream().emit("agent_activity", activity("p1", 0));
    });
    await settle();
    reset();
    await act(async () => {
      for (let i = 1; i <= 20; i += 1) stream().emit("agent_activity", activity("p1", i));
    });
    await settle();
    expect(renders.topBar).toBe(0);
    expect(renders.frames).toEqual({});
    expect(renders.spaceReader).toBe(0);
    expect(renders.activityReader).toBeGreaterThan(0);
    expect(document.querySelector("[data-activity-count]")?.textContent).toBe("1");
  });

  it("eventos de outro projeto não re-renderizam a barra, as molduras nem quem usa useSpace()", async () => {
    const { stream, server } = await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja"), makeProject("p2", "Blog")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja" }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
      apps: { conversation: { Content: ActivityReader, scroll: "app" }, plan: { Content: SpaceReader, scroll: "app" } },
    });
    await settle();
    reset();
    const attentionBefore = server.count("GET", /^\/api\/attention$/);
    await act(async () => {
      stream().emit("agent_activity", activity("p2", 1));
      stream().emit("task_updated", {
        event_type: "task_updated",
        project_id: "p2",
        payload: { spec_id: "s1", task_id: "T01", status: "IN_PROGRESS" },
      });
      stream().emit("project_status", {
        event_type: "project_status",
        project_id: "p2",
        payload: { status: "executing", spec_id: "s1" },
      });
    });
    await settle();
    // O /api/attention foi relido (evento relevante), mas veio igual: nada re-renderiza.
    expect(server.count("GET", /^\/api\/attention$/)).toBeGreaterThan(attentionBefore);
    expect(renders.topBar).toBe(0);
    expect(renders.frames).toEqual({});
    expect(renders.spaceReader).toBe(0);
    expect(renders.activityReader).toBe(0);
  });

  it("janelas reais (Conversa e Plano da T06): a rajada só re-renderiza a Conversa; outro projeto, nada", async () => {
    const plan = makeSpec("s1", "Loja", "AWAITING_APPROVAL", [makeTask("T01", "Base", "TODO")]);
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja"), makeProject("p2", "Blog")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [plan] }), p2: makeProjectState({ id: "p2", name: "Blog" }) },
    });
    expect(renders.conversation).toBeGreaterThan(0);
    expect(renders.plan).toBeGreaterThan(0);
    await act(async () => {
      stream().emit("agent_activity", activity("p1", 0));
    });
    await settle();
    reset();
    await act(async () => {
      for (let i = 1; i <= 20; i += 1) stream().emit("agent_activity", activity("p1", i));
    });
    await settle();
    expect(renders.topBar).toBe(0);
    expect(renders.frames).toEqual({});
    expect(renders.plan).toBe(0);
    expect(renders.conversation).toBeGreaterThan(0);
    expect(renders.conversation).toBeLessThanOrEqual(21);

    reset();
    await act(async () => {
      stream().emit("agent_activity", activity("p2", 1));
      stream().emit("agent_message", { event_type: "agent_message", project_id: "p2", payload: { agent: "architect", text: "oi", turn_id: "t2" } });
    });
    await settle();
    expect(renders.topBar).toBe(0);
    expect(renders.frames).toEqual({});
    expect(renders.conversation).toBe(0);
    expect(renders.plan).toBe(0);
  });

  it("Quadro e Atividade reais (T07): a rajada não re-renderiza o Quadro; a Atividade, uma vez por quadro", async () => {
    const scheduler = manualScheduler();
    const spec = makeSpec("s1", "Loja", "IN_PROGRESS", [makeTask("T01", "Base", "DONE"), makeTask("T02", "Carrinho", "IN_PROGRESS")]);
    const { stream } = await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja"), makeProject("p2", "Blog")],
      states: {
        p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec], running: true, run: { spec_id: "s1", status: "running" } }),
        p2: makeProjectState({ id: "p2", name: "Blog" }),
      },
      scheduler,
    });
    expect(document.querySelector('[data-window="board"] [data-board]')).not.toBeNull();
    expect(document.querySelector('[data-window="activity"] [data-activity-window]')).not.toBeNull();
    await act(async () => {
      stream().emit("agent_activity", activity("p1", 0));
      scheduler.runFrames();
    });
    await settle();
    await act(async () => {
      scheduler.runFrames();
    });
    reset();
    await act(async () => {
      for (let i = 1; i <= 20; i += 1) stream().emit("agent_activity", activity("p1", i));
    });
    await settle();
    // Antes do quadro, nem a Atividade re-renderiza; o Quadro, nunca.
    expect(renders.board).toBe(0);
    expect(renders.activity).toBe(0);
    await act(async () => {
      scheduler.runFrames();
    });
    await settle();
    expect(renders.board).toBe(0);
    expect(renders.activity).toBe(1);
    expect(renders.topBar).toBe(0);
    expect(renders.frames).toEqual({});
    expect(document.querySelector('[data-agent-card="developer"] [data-live-detail]')?.textContent).toBe("passo 20");

    // Outro projeto: nada re-renderiza.
    reset();
    await act(async () => {
      stream().emit("agent_activity", activity("p2", 1));
      scheduler.runFrames();
    });
    await settle();
    expect(renders.board).toBe(0);
    expect(renders.activity).toBe(0);
  });

  it("Quadro (T07): re-renderiza quando muda o que ele mostra (o estado da tarefa)", async () => {
    const spec = makeSpec("s1", "Loja", "IN_PROGRESS", [makeTask("T01", "Base", "IN_PROGRESS")]);
    const { server } = await renderShell({
      url: "/?space=p1",
      projects: [makeProject("p1", "Loja")],
      states: { p1: makeProjectState({ id: "p1", name: "Loja", specs: [spec], running: true, run: { spec_id: "s1", status: "running" } }) },
    });
    reset();
    server.model.states.p1 = makeProjectState({
      id: "p1",
      name: "Loja",
      specs: [makeSpec("s1", "Loja", "IN_PROGRESS", [makeTask("T01", "Base", "IN_REVIEW")])],
      running: true,
      run: { spec_id: "s1", status: "running" },
    });
    await act(async () => {
      appStore.setState((st) => ({ revisions: { ...st.revisions, p1: (st.revisions.p1 ?? 0) + 1 } }));
    });
    await settle();
    expect(renders.board).toBeGreaterThan(0);
    expect(document.querySelector('[data-column="IN_REVIEW"] [data-task-id="T01"]')).not.toBeNull();
  });
});
