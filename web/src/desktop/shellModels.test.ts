import { afterEach, describe, expect, it, vi } from "vitest";
import type { ApiEvent } from "../api/types";
import { installFetch } from "../test-utils";
import {
  attentionCount,
  createProjectByName,
  GIT_MISSING_MESSAGE,
  NAME_INVALID_MESSAGE,
  parseAttention,
  parseReadiness,
  requiredMissing,
  setupIncomplete,
} from "./api";
import { createAttentionFeed, isAttentionEvent } from "./attentionFeed";
import { needsNormalize, parseSpaceLocation, spaceUrl } from "./spaceRoute";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("/api/attention (parser e contagem)", () => {
  it("valida campo a campo e descarta o que não confere", () => {
    const parsed = parseAttention({
      setup_incomplete: true,
      projects: [
        {
          project_id: "p1",
          name: "Loja",
          pending_permissions: [
            { request_id: "r1", agent: "developer", kind: "command", task_id: "T01", always_allowed: false },
            { request_id: "r2", agent: "hacker", kind: "command" },
          ],
          pending_choices: true,
          plan_awaiting_approval: false,
          paused_or_stalled: { kind: "paused", spec_id: "s1", task_id: "T02", escalation_options: [{ id: "o1", kind: "guidance" }, { id: "o2", kind: "x" }] },
        },
        { name: "sem id" },
      ],
    });
    expect(parsed?.projects).toHaveLength(1);
    expect(parsed?.projects[0]?.pending_permissions.map((p) => p.request_id)).toEqual(["r1"]);
    expect(parsed?.projects[0]?.paused_or_stalled).toEqual({
      kind: "paused",
      spec_id: "s1",
      task_id: "T02",
      escalation_options: [{ id: "o1", kind: "guidance" }],
    });
    // 1 permissão + escolhas + pausa + setup.
    expect(attentionCount(parsed)).toBe(4);
    expect(parseAttention({ projects: "x" })).toBeNull();
    expect(attentionCount(null)).toBe(0);
  });
});

describe("/api/readiness (parser)", () => {
  it("níveis obrigatórios e pendências sem repetição", () => {
    const item = { id: "connection", label: "Conexão", settings_tab: "connections" };
    const parsed = parseReadiness({
      chat: { ok: false, missing: [item] },
      create_project: { ok: false, missing: [item, { id: "git", label: "Git", fix: "sudo apt install git", settings_tab: "machine" }] },
      execute: { ok: true, missing: [] },
    });
    if (parsed === null) throw new Error("inválido");
    expect(parsed.recommended).toEqual({ ok: true, missing: [] });
    expect(requiredMissing(parsed).map((i) => i.id)).toEqual(["connection", "git"]);
    expect(setupIncomplete(parsed)).toBe(true);
    expect(parseReadiness({ chat: { ok: true } })).toBeNull();
  });
});

describe("POST /api/projects {name}", () => {
  it("409 project_dir_exists devolve o caminho; 400 e 412 viram mensagens fixas", async () => {
    const replies = [
      { status: 409, body: { error: "project_dir_exists", path: "/home/u/NantsProjects/loja" } },
      { status: 400, body: { error: "nome inválido: use letras ou números" } },
      { status: 412, body: { error: "prereq_missing", missing: ["git"] } },
    ];
    installFetch(() => replies.shift() ?? { status: 500 });
    expect(await createProjectByName("Loja")).toEqual({ kind: "exists", path: "/home/u/NantsProjects/loja" });
    expect(await createProjectByName("!!")).toMatchObject({ kind: "error", message: NAME_INVALID_MESSAGE });
    expect(await createProjectByName("Loja")).toMatchObject({ kind: "error", message: GIT_MISSING_MESSAGE });
  });
});

describe("alimentador do /api/attention", () => {
  it("no máximo 1 consulta a cada 500 ms; pedidos na janela viram uma no fim dela", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(0);
    const load = vi.fn(async () => ({ n: 1 }));
    const onData = vi.fn();
    const feed = createAttentionFeed({ load, onData, minIntervalMs: 500 });
    feed.request();
    await vi.advanceTimersByTimeAsync(0);
    expect(load).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 10; i += 1) feed.request();
    await vi.advanceTimersByTimeAsync(100);
    expect(load).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(400);
    expect(load).toHaveBeenCalledTimes(2);
    expect(onData).toHaveBeenCalledTimes(2);
    feed.dispose();
    feed.request();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("pedido durante a consulta em voo agenda mais uma depois dela", async () => {
    vi.useFakeTimers();
    let resolve: (v: number) => void = () => undefined;
    const load = vi.fn(() => new Promise<number>((r) => (resolve = r)));
    const feed = createAttentionFeed({ load, onData: () => undefined, minIntervalMs: 500 });
    feed.request();
    feed.request();
    expect(load).toHaveBeenCalledTimes(1);
    resolve(1);
    await vi.advanceTimersByTimeAsync(600);
    expect(load).toHaveBeenCalledTimes(2);
    feed.dispose();
  });

  it("relógio que volta (ajuste do sistema, WSL2) nunca espera mais que o intervalo", async () => {
    vi.useFakeTimers();
    let clock = 10_000;
    const load = vi.fn(() => Promise.resolve(1));
    const feed = createAttentionFeed({ load, onData: () => undefined, minIntervalMs: 500, now: () => clock });
    feed.request();
    await vi.advanceTimersByTimeAsync(0);
    expect(load).toHaveBeenCalledTimes(1);
    // O relógio salta 2,3 s para trás: a próxima consulta sai em até 500 ms, não em 2,8 s.
    clock -= 2_300;
    feed.request();
    await vi.advanceTimersByTimeAsync(500);
    expect(load).toHaveBeenCalledTimes(2);
    feed.dispose();

    // Sem intervalo (como nos testes do shell), a consulta sai na hora mesmo com o relógio para trás.
    let back = 50_000;
    const instant = vi.fn(() => Promise.resolve(1));
    const zero = createAttentionFeed({ load: instant, onData: () => undefined, minIntervalMs: 0, now: () => back });
    zero.request();
    await vi.advanceTimersByTimeAsync(0);
    back -= 2_300;
    zero.request();
    expect(instant).toHaveBeenCalledTimes(2);
    zero.dispose();
  });

  it("eventos relevantes", () => {
    const ev = (event_type: string): ApiEvent => ({ event_type, project_id: "p1", payload: {} }) as unknown as ApiEvent;
    expect(isAttentionEvent(ev("permission_requested"))).toBe(true);
    expect(isAttentionEvent(ev("task_updated"))).toBe(true);
    expect(isAttentionEvent(ev("agent_activity"))).toBe(false);
    expect(isAttentionEvent(ev("agent_message"))).toBe(false);
  });
});

describe("URL do espaço", () => {
  it("só /?space=<id>; o resto é normalizado", () => {
    expect(spaceUrl(null)).toBe("/");
    expect(spaceUrl("p1")).toBe("/?space=p1");
    expect(parseSpaceLocation("?space=p1")).toEqual({ space: "p1", canonical: "/?space=p1" });
    expect(parseSpaceLocation("")).toEqual({ space: null, canonical: "/" });
    expect(parseSpaceLocation("?space=../x")).toEqual({ space: null, canonical: "/" });
    expect(needsNormalize("/", "?space=p1")).toBeNull();
    expect(needsNormalize("/", "")).toBeNull();
    expect(needsNormalize("/outro", "")).toBe("/");
    expect(needsNormalize("/", "?space=p1&x=2")).toBe("/?space=p1");
    expect(needsNormalize("/", "?space=p1", "#a")).toBe("/?space=p1");
  });
});
