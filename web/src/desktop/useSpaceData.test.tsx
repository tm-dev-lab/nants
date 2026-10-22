import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { appStore } from "../store";
import { installFetch, makeConfig, makeProject, makeProjectState, makeSpec, makeTask, resetStore } from "../test-utils";
import type { ProjectState } from "../api/types";
import {
  ACTIVITY_PRUNE_MS,
  PLAN_REVIEW_RECHECK_MS,
  REMOTE_TURN_EXPIRY_MS,
  TRACKING_POLL_MS,
  useActivityPrune,
  useSpaceActivities,
  useSpaceData,
} from "./useSpaceData";

let stateCalls = 0;
let chatCalls = 0;
let stateVersion = 0;
/** Estado devolvido pelo GET (padrão: projeto vazio com o nome da versão). */
let stateBody: ProjectState | null = null;

beforeEach(() => {
  stateCalls = 0;
  chatCalls = 0;
  stateVersion = 0;
  stateBody = null;
  resetStore({ config: makeConfig([makeProject("p1", "Loja")]) });
  installFetch((_method, url) => {
    if (/\/state$/.test(url)) {
      stateCalls += 1;
      return { body: stateBody ?? makeProjectState({ id: "p1", name: `Loja ${stateVersion}` }) };
    }
    if (/\/chat$/.test(url)) {
      chatCalls += 1;
      return { body: { turns: [] } };
    }
    if (/\/activity$/.test(url)) return { body: [] };
    return { status: 404, body: {} };
  });
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

async function flush(): Promise<void> {
  await act(async () => {
    for (let i = 0; i < 10; i += 1) await Promise.resolve();
  });
}

describe("useSpaceData", () => {
  it("carrega estado, histórico e atividades uma vez; deriva fase, plano e execução", async () => {
    const { result } = renderHook(() => useSpaceData("p1"));
    await flush();
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(stateCalls).toBe(1);
    expect(chatCalls).toBe(1);
    expect(result.current.status).toBe("ready");
    expect(result.current.name).toBe("Loja 0");
    expect(result.current.category).toBe("planning");
    expect(appStore.getState().activeProjectId).toBe("p1");

    const plan = makeSpec("s1", "Plano", "AWAITING_APPROVAL", [makeTask("T01", "a", "TODO")]);
    act(() => {
      appStore.setState((s) => ({ projectStates: { ...s.projectStates, p1: makeProjectState({ id: "p1", specs: [plan] }) } }));
    });
    expect(result.current.planReady?.id).toBe("s1");
    expect(result.current.planStage).toBe("awaiting");
    expect(result.current.phase).toBe("waiting_approval");
  });

  it("etapa do plano (094): turno aberto = planejando, sem plano pronto; revisão = revisando, sem plano pronto", async () => {
    const { result } = renderHook(() => useSpaceData("p1"));
    await flush();
    const plan = makeSpec("s1", "Plano", "AWAITING_APPROVAL", [makeTask("T01", "a", "TODO")]);
    act(() => {
      appStore.setState((s) => ({
        projectStates: { ...s.projectStates, p1: makeProjectState({ id: "p1", specs: [plan] }) },
        architectTurns: { p1: { turnId: "t1", startedAt: Date.now() } },
      }));
    });
    expect(result.current.planStage).toBe("planning");
    expect(result.current.planReady).toBeNull();
    expect(result.current.phase).toBe("planning");
    const reviewing = { ...plan, plan_review: { status: "running", issues: [] } };
    act(() => {
      appStore.setState((s) => ({
        projectStates: { ...s.projectStates, p1: makeProjectState({ id: "p1", specs: [reviewing] }) },
        architectTurns: {},
      }));
    });
    expect(result.current.planStage).toBe("reviewing");
    expect(result.current.planReady).toBeNull();
    expect(result.current.phase).toBe("reviewing_plan");
  });

  it("revisão do plano pending/running: o ESPAÇO reconsulta o estado a cada 20 s até ela terminar (janela Plano aberta ou não)", async () => {
    vi.useFakeTimers();
    const plan = makeSpec("s1", "Plano", "AWAITING_APPROVAL", [makeTask("T01", "a", "TODO")]);
    stateBody = makeProjectState({ id: "p1", specs: [{ ...plan, plan_review: { status: "pending", issues: [] } }] });
    const { result } = renderHook(() => useSpaceData("p1"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(result.current.planStage).toBe("reviewing");
    const before = stateCalls;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(PLAN_REVIEW_RECHECK_MS * 2);
    });
    expect(stateCalls).toBe(before + 2);
    // A revisão termina: a próxima consulta traz `ok`, a etapa vira "awaiting" e o poll para.
    stateBody = makeProjectState({ id: "p1", specs: [{ ...plan, plan_review: { status: "ok", issues: [] } }] });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(PLAN_REVIEW_RECHECK_MS);
    });
    expect(result.current.planStage).toBe("awaiting");
    expect(result.current.planReady?.id).toBe("s1");
    const after = stateCalls;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(PLAN_REVIEW_RECHECK_MS * 3);
    });
    expect(stateCalls).toBe(after);
  });

  it("revisions e liveRevisions: 1 GET de estado por aviso", async () => {
    renderHook(() => useSpaceData("p1"));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    const before = stateCalls;
    act(() => {
      appStore.setState((s) => ({ revisions: { ...s.revisions, p1: 1 } }));
    });
    act(() => {
      appStore.setState((s) => ({ liveRevisions: { ...s.liveRevisions, p1: 1 } }));
    });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(stateCalls).toBe(before + 2);
  });

  it("projeto ausente da config e sem estado: não encontrado; sem config: carregando", async () => {
    resetStore();
    const { result } = renderHook(() => useSpaceData("zz"));
    expect(result.current.status).toBe("loading");
    act(() => {
      appStore.setState({ config: makeConfig([]) });
    });
    expect(result.current.status).toBe("not_found");
  });

  it("poda das atividades a cada 500 ms enquanto houver alguma (useActivityPrune)", async () => {
    vi.useFakeTimers();
    renderHook(() => useActivityPrune("p1"));
    const now = Date.now();
    act(() => {
      appStore.setState({
        activities: {
          p1: [{ key: "developer:T01", agent: "developer", activityId: "a", reasoning: "", text: "", tool: "", startedAt: now - 60_000, lastAt: now - 60_000, endedAt: now - 30_000 }],
        },
      });
    });
    expect(appStore.getState().activities.p1).toHaveLength(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ACTIVITY_PRUNE_MS);
    });
    expect(appStore.getState().activities.p1 ?? []).toHaveLength(0);
  });

  it("atividades e mensagens ficam fora de SpaceData: uma atividade não muda o objeto do espaço", async () => {
    const { result } = renderHook(() => ({ space: useSpaceData("p1"), activities: useSpaceActivities("p1") }));
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    const before = result.current.space;
    expect("activities" in before).toBe(false);
    expect("messages" in before).toBe(false);
    act(() => {
      appStore.applyEvent({
        event_type: "agent_activity",
        project_id: "p1",
        payload: { agent: "developer", activity_id: "a1", kind: "reasoning", text: "pensando" },
      } as never);
    });
    expect(result.current.activities).toHaveLength(1);
    expect(result.current.space).toBe(before);
  });

  it("execução exige spec ativa (como no ProjectEntry)", async () => {
    resetStore({ config: makeConfig([makeProject("p1", "Loja")]) });
    act(() => {
      appStore.setState({ projectStates: { p1: makeProjectState({ id: "p1", running: true, specs: [] }) } });
    });
    const { result } = renderHook(() => useSpaceData("p1"));
    expect(result.current.execution).toBe(false);
  });

  it("rastreio do turno: consulta o estado a cada 10 s e, quando o turno acaba, recarrega o histórico", async () => {
    vi.useFakeTimers();
    const { result } = renderHook(() => useSpaceData("p1"));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const chatBefore = chatCalls;
    act(() => {
      appStore.setState({ architectTurns: { p1: { turnId: "abc", startedAt: Date.now() } } });
    });
    act(() => result.current.trackTurn(null));
    expect(result.current.tracking).toBe(true);
    const before = stateCalls;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(TRACKING_POLL_MS);
    });
    expect(stateCalls).toBe(before + 1);
    // O estado lido depois da queda não tem turno: o acompanhamento termina com o histórico.
    stateVersion = 1;
    act(() => {
      appStore.endArchitectTurn("p1", appStore.getState().architectTurns.p1?.turnId ?? "abc");
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(TRACKING_POLL_MS);
    });
    expect(chatCalls).toBeGreaterThan(chatBefore);
    expect(result.current.tracking).toBe(false);
  });

  it("rede de segurança: o turno remoto expira em 210 s sem chat_turn_ended", async () => {
    vi.useFakeTimers();
    renderHook(() => useSpaceData("p1"));
    act(() => {
      appStore.setState({ architectTurns: { p1: { turnId: "t1", startedAt: Date.now() } } });
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(REMOTE_TURN_EXPIRY_MS);
    });
    expect(appStore.getState().architectTurns.p1).toBeUndefined();
  });
});
