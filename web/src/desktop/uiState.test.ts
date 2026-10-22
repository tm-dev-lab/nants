import { describe, expect, it, vi } from "vitest";
import { request } from "../api/client";
import {
  getSection,
  isSectionAllowed,
  parseEnvelope,
  parseSelection,
  setSection,
  UI_SECTIONS,
  UI_STATE_DEBOUNCE_MS,
  UI_STATE_RETRY_MS,
  uiStateCacheKey,
  UiStateSync,
  type UiScope,
} from "./uiState";
import { fakeUiStateServer, memoryBroadcastHub, memoryStorage } from "./test-utils";

const PROJECT: UiScope = { kind: "project", projectId: "p1" };
const CURRENT = UI_SECTIONS.layout("execution", "current");

/** Timers manuais: `advance(ms)` dispara os vencidos. */
function manualTimers() {
  let now = 0;
  let seq = 0;
  const timers = new Map<number, { at: number; fn: () => void; ms: number }>();
  return {
    setTimer: (fn: () => void, ms: number) => {
      seq += 1;
      timers.set(seq, { at: now + ms, fn, ms });
      return seq;
    },
    clearTimer: (handle: unknown) => {
      timers.delete(handle as number);
    },
    delays: () => [...timers.values()].map((t) => t.ms),
    async advance(ms: number) {
      now += ms;
      for (const [id, t] of [...timers]) {
        if (t.at <= now) {
          timers.delete(id);
          t.fn();
        }
      }
      await settle();
    },
  };
}

/** Deixa as promessas do fetch/PUT assentarem. */
async function settle(): Promise<void> {
  for (let i = 0; i < 20; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
}

function makeSync(server: ReturnType<typeof fakeUiStateServer>, extra: Partial<ConstructorParameters<typeof UiStateSync>[1]> = {}) {
  const timers = manualTimers();
  const sync = new UiStateSync(PROJECT, {
    fetchImpl: server.fetchImpl,
    storage: memoryStorage(),
    createChannel: null,
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer,
    ...extra,
  });
  return { sync, timers };
}

describe("seções e caminhos", () => {
  it("seções permitidas por escopo", () => {
    expect(isSectionAllowed({ kind: "global" }, "appearance")).toBe(true);
    expect(isSectionAllowed({ kind: "global" }, "hints")).toBe(true);
    expect(isSectionAllowed({ kind: "global" }, "network")).toBe(true);
    expect(isSectionAllowed(PROJECT, "network")).toBe(false);
    expect(isSectionAllowed({ kind: "global" }, "language")).toBe(true);
    expect(isSectionAllowed(PROJECT, "language")).toBe(false);
    expect(isSectionAllowed({ kind: "global" }, "selection")).toBe(false);
    expect(isSectionAllowed(PROJECT, "layouts.planning.mine")).toBe(true);
    expect(isSectionAllowed(PROJECT, "layouts.done.current")).toBe(true);
    expect(isSectionAllowed(PROJECT, "selection")).toBe(true);
    expect(isSectionAllowed(PROJECT, "mobile")).toBe(true);
    expect(isSectionAllowed(PROJECT, "layouts.revisao.current")).toBe(false);
    expect(isSectionAllowed(PROJECT, "appearance")).toBe(false);
  });

  it("setSection preserva as irmãs e recusa __proto__", () => {
    const data = { layouts: { planning: { mine: 1 }, done: { current: 2 } }, selection: { taskId: "T1" } };
    const next = setSection(data, "layouts.planning.current", { a: 1 });
    expect(next).toEqual({ layouts: { planning: { mine: 1, current: { a: 1 } }, done: { current: 2 } }, selection: { taskId: "T1" } });
    expect(data.layouts.planning).toEqual({ mine: 1 });
    expect(setSection(data, "__proto__.polluted", true)).toBe(data);
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();
    expect(getSection(next, "layouts.planning.current")).toEqual({ a: 1 });
    expect(getSection(next, "layouts.nada.current")).toBeUndefined();
    expect(getSection(next, "constructor")).toBeUndefined();
  });

  it("envelope inválido ou de outra versão volta ao padrão sem erro", () => {
    expect(parseEnvelope({ version: 1, rev: 3, data: { a: 1 } })).toEqual({ version: 1, rev: 3, data: { a: 1 } });
    expect(parseEnvelope({ version: 0, rev: 3, data: { a: 1 } })).toEqual({ version: 1, rev: 3, data: {} });
    expect(parseEnvelope({ version: 1, rev: -1, data: [] })).toEqual({ version: 1, rev: 0, data: {} });
    expect(parseEnvelope("lixo")).toEqual({ version: 1, rev: 0, data: {} });
  });

  it("parseSelection: tarefa, arquivo e rolagem; inválido vira null", () => {
    expect(parseSelection({ taskId: "T03", filePath: "src/a.ts", conversationScroll: 120 })).toEqual({
      taskId: "T03",
      filePath: "src/a.ts",
      conversationScroll: 120,
    });
    expect(parseSelection({ taskId: 3, filePath: "", conversationScroll: -1 })).toEqual({
      taskId: null,
      filePath: null,
      conversationScroll: null,
    });
    expect(parseSelection(undefined)).toEqual({ taskId: null, filePath: null, conversationScroll: null });
  });

  it("parseSelection: colunas do Quadro e aba da Atividade (T07); lixo é descartado", () => {
    expect(parseSelection({ boardColumns: ["APPROVED", 3, "", "APPROVED", "BLOCKED"], activityTab: "history" })).toEqual({
      taskId: null,
      filePath: null,
      conversationScroll: null,
      boardColumns: ["APPROVED", "BLOCKED"],
      activityTab: "history",
    });
    const junk = parseSelection({ boardColumns: "APPROVED", activityTab: "outra" });
    expect(junk.boardColumns).toBeUndefined();
    expect(junk.activityTab).toBeUndefined();
  });

  it("parseSelection: tamanho da tela do App (T09); valor desconhecido é descartado", () => {
    expect(parseSelection({ appViewport: "tablet" }).appViewport).toBe("tablet");
    expect(parseSelection({ appViewport: "phone" }).appViewport).toBe("phone");
    expect(parseSelection({ appViewport: "4k" }).appViewport).toBeUndefined();
    expect(parseSelection({ appViewport: 768 }).appViewport).toBeUndefined();
  });
});

describe("UiStateSync", () => {
  it("carrega do servidor e marca pronto", async () => {
    const server = fakeUiStateServer({ rev: 4, data: { selection: { taskId: "T2" } } });
    const { sync } = makeSync(server);
    await sync.start();
    expect(sync.getSnapshot()).toMatchObject({ rev: 4, ready: true });
    expect(parseSelection(sync.get("selection")).taskId).toBe("T2");
    expect(server.calls[0]?.url).toBe("/api/projects/p1/ui-state");
  });

  it("estado de versão antiga no servidor vira o padrão sem erro", async () => {
    const server = fakeUiStateServer({ version: 0, rev: 2, data: { layouts: "velho" } });
    const { sync } = makeSync(server);
    await sync.start();
    expect(sync.getSnapshot()).toMatchObject({ rev: 2, data: {}, ready: true });
  });

  it("debounce: várias mudanças em 1 s viram um PUT por seção, com base_rev", async () => {
    const server = fakeUiStateServer({ rev: 1 });
    const { sync, timers } = makeSync(server);
    await sync.start();
    sync.set(CURRENT, { windows: { a: 1 } });
    sync.set(CURRENT, { windows: { a: 2 } });
    sync.set("selection", { taskId: "T1" });
    expect(server.puts()).toHaveLength(0);
    expect(timers.delays()).toEqual([UI_STATE_DEBOUNCE_MS]);
    await timers.advance(UI_STATE_DEBOUNCE_MS - 1);
    expect(server.puts()).toHaveLength(0);
    await timers.advance(1);
    expect(server.puts().map((c) => c.body)).toEqual([
      { base_rev: 1, section: CURRENT, value: { windows: { a: 2 } } },
      { base_rev: 2, section: "selection", value: { taskId: "T1" } },
    ]);
    expect(sync.getSnapshot().rev).toBe(3);
    expect(sync.hasPending()).toBe(false);
  });

  it("409: relê o envelope e reaplica só a própria seção (sem apagar a do outro escritor)", async () => {
    const server = fakeUiStateServer({ rev: 1 });
    const { sync, timers } = makeSync(server);
    await sync.start();
    server.externalWrite("mobile", { tab: "board" });
    sync.set(CURRENT, { windows: {} });
    await timers.advance(UI_STATE_DEBOUNCE_MS);
    const puts = server.puts();
    expect(puts).toHaveLength(2);
    expect(puts[0]?.body).toMatchObject({ base_rev: 1 });
    expect(puts[1]?.body).toMatchObject({ base_rev: 2, section: CURRENT });
    expect(server.state.data).toEqual({ mobile: { tab: "board" }, layouts: { execution: { current: { windows: {} } } } });
    expect(sync.get("mobile")).toEqual({ tab: "board" });
    expect(sync.getSnapshot().rev).toBe(3);
  });

  it("falha de rede/5xx mantém pendente e tenta de novo depois; 422 descarta", async () => {
    const server = fakeUiStateServer();
    const { sync, timers } = makeSync(server);
    await sync.start();
    server.forceNext(503);
    sync.set("selection", { taskId: "T1" });
    await timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(sync.hasPending()).toBe(true);
    expect(timers.delays()).toEqual([UI_STATE_RETRY_MS]);
    await timers.advance(UI_STATE_RETRY_MS);
    expect(sync.hasPending()).toBe(false);
    expect(server.state.data).toEqual({ selection: { taskId: "T1" } });

    server.forceNext(422);
    sync.set("mobile", { x: 1 });
    await timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(sync.hasPending()).toBe(false);
    expect(timers.delays()).toEqual([]);
  });

  it("seção não permitida e celular fora de `mobile` são ignorados", async () => {
    const server = fakeUiStateServer();
    const { sync } = makeSync(server);
    expect(sync.set("appearance", { theme: "dark" })).toBe(false);
    const mobile = makeSync(server, { mobileOnly: true }).sync;
    expect(mobile.set(CURRENT, { windows: {} })).toBe(false);
    expect(mobile.set("mobile", { tab: "plan" })).toBe(true);
  });

  it("girar o aparelho liga e desliga o modo celular sem recriar o cliente (T10)", () => {
    const server = fakeUiStateServer();
    const { sync } = makeSync(server);
    expect(sync.isMobileOnly()).toBe(false);
    sync.setMobileOnly(true);
    expect(sync.isMobileOnly()).toBe(true);
    expect(sync.set(CURRENT, { windows: {} })).toBe(false);
    expect(sync.set("selection", { taskId: "T01" })).toBe(false);
    expect(sync.get(CURRENT)).toBeUndefined();
    expect(sync.set("mobile", { view: "plan" })).toBe(true);
    sync.setMobileOnly(false);
    expect(sync.set(CURRENT, { windows: {} })).toBe(true);
  });

  it("pagehide: keepalive só com as seções alteradas", async () => {
    const server = fakeUiStateServer({ rev: 7, data: { selection: { taskId: "T1" } } });
    const { sync, timers } = makeSync(server);
    await sync.start();
    sync.set(CURRENT, { windows: {} });
    sync.flushOnPageHide();
    await settle();
    const puts = server.puts();
    expect(puts).toHaveLength(1);
    expect(puts[0]).toMatchObject({ keepalive: true, body: { base_rev: 7, section: CURRENT, value: { windows: {} } } });
    expect(timers.delays()).toEqual([]);
    expect(sync.hasPending()).toBe(false);
    // Sem nada pendente, o pagehide não manda nada.
    sync.flushOnPageHide();
    await settle();
    expect(server.puts()).toHaveLength(1);
  });

  it("pagehide com duas seções pendentes: base_rev encadeado e as duas chegam ao servidor", async () => {
    const server = fakeUiStateServer({ rev: 7 });
    const { sync } = makeSync(server);
    await sync.start();
    sync.set("selection", { taskId: "T1" });
    sync.set(CURRENT, { windows: {} });
    sync.flushOnPageHide();
    await settle();
    const puts = server.puts();
    // Ordem determinística (por nome da seção) e rev encadeado.
    expect(puts.map((c) => c.body)).toEqual([
      { base_rev: 7, section: CURRENT, value: { windows: {} } },
      { base_rev: 8, section: "selection", value: { taskId: "T1" } },
    ]);
    expect(puts.every((c) => c.keepalive)).toBe(true);
    expect(server.state.rev).toBe(9);
    expect(server.state.data).toEqual({ selection: { taskId: "T1" }, layouts: { execution: { current: { windows: {} } } } });
  });

  it("pagehide com PUT em voo: não reenvia a seção em voo e encadeia depois dele", async () => {
    const server = fakeUiStateServer({ rev: 3 });
    let release: () => void = () => undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let held = false;
    // O servidor processa o primeiro PUT na hora, mas a resposta demora (ainda "em voo" no cliente).
    const fetchImpl = (async (input: string | URL | Request, init?: RequestInit) => {
      const response = await server.fetchImpl(input, init);
      if (init?.method === "PUT" && !held) {
        held = true;
        await gate;
      }
      return response;
    }) as typeof fetch;
    const { sync, timers } = makeSync(server, { fetchImpl });
    await sync.start();
    sync.set("selection", { taskId: "T1" });
    await timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(held).toBe(true);
    sync.set("mobile", { tab: "plan" });
    sync.flushOnPageHide();
    await settle();
    release();
    await settle();
    const puts = server.puts();
    expect(puts.map((c) => [c.body, c.keepalive])).toEqual([
      [{ base_rev: 3, section: "selection", value: { taskId: "T1" } }, true],
      [{ base_rev: 4, section: "mobile", value: { tab: "plan" } }, true],
    ]);
    expect(server.state.rev).toBe(5);
    expect(server.state.data).toEqual({ selection: { taskId: "T1" }, mobile: { tab: "plan" } });
  });

  /**
   * Simula o fechamento da aba: `visibilitychange` (flush normal) e logo `pagehide`, e então o
   * documento descarrega. Um fetch sem `keepalive` em voo é ABORTADO antes de chegar ao servidor;
   * com `keepalive`, chega ao servidor (a resposta é que se perde).
   */
  function unloadingPage(server: ReturnType<typeof fakeUiStateServer>) {
    let unload: () => void = () => undefined;
    const unloaded = new Promise<void>((resolve) => {
      unload = resolve;
    });
    let dead = false;
    const keepaliveFlags: boolean[] = [];
    const abort = (): never => {
      throw new DOMException("documento descarregado", "AbortError");
    };
    const fetchImpl = (async (input: string | URL | Request, init?: RequestInit) => {
      if (dead) return abort();
      if (init?.method !== "PUT") return server.fetchImpl(input, init);
      keepaliveFlags.push(init.keepalive === true);
      if (init.keepalive === true) {
        const response = await server.fetchImpl(input, init);
        await unloaded;
        return dead ? abort() : response;
      }
      await unloaded;
      return abort();
    }) as typeof fetch;
    return {
      fetchImpl,
      keepaliveFlags,
      unload: () => {
        dead = true;
        unload();
      },
    };
  }

  it("fechar a aba com uma seção em voo pelo flush normal: o PUT é keepalive e chega ao servidor", async () => {
    const server = fakeUiStateServer({ rev: 4 });
    const page = unloadingPage(server);
    const { sync } = makeSync(server, { fetchImpl: page.fetchImpl });
    await sync.start();
    sync.set("selection", { taskId: "T1" });
    void sync.flush(); // visibilitychange → hidden
    await settle();
    sync.flushOnPageHide(); // pagehide: a seção em voo não é reenviada
    page.unload();
    await settle();
    expect(page.keepaliveFlags).toEqual([true]);
    expect(server.puts().map((c) => [c.body, c.keepalive])).toEqual([[{ base_rev: 4, section: "selection", value: { taskId: "T1" } }, true]]);
    expect(server.state).toMatchObject({ rev: 5, data: { selection: { taskId: "T1" } } });
  });

  it("fechar a aba com duas seções (uma em voo, outra no pagehide): as duas chegam, sem 409", async () => {
    const server = fakeUiStateServer({ rev: 4 });
    const page = unloadingPage(server);
    const { sync } = makeSync(server, { fetchImpl: page.fetchImpl });
    await sync.start();
    sync.set("selection", { taskId: "T1" });
    sync.set(CURRENT, { windows: {} });
    void sync.flush();
    await settle();
    sync.flushOnPageHide();
    page.unload();
    await settle();
    expect(page.keepaliveFlags).toEqual([true, true]);
    expect(server.puts().map((c) => c.body)).toEqual([
      { base_rev: 4, section: "selection", value: { taskId: "T1" } },
      { base_rev: 5, section: CURRENT, value: { windows: {} } },
    ]);
    expect(server.state.rev).toBe(6);
    expect(server.state.data).toEqual({ selection: { taskId: "T1" }, layouts: { execution: { current: { windows: {} } } } });
  });

  it("stop() cancela a nova tentativa pendente depois de 5xx (também se o PUT terminar depois)", async () => {
    const server = fakeUiStateServer();
    const { sync, timers } = makeSync(server);
    await sync.start();
    server.forceNext(503);
    sync.set("selection", { taskId: "T1" });
    await timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(timers.delays()).toEqual([UI_STATE_RETRY_MS]);
    sync.stop();
    expect(timers.delays()).toEqual([]);

    const other = makeSync(server);
    await other.sync.start();
    server.forceNext(503);
    other.sync.set("mobile", { x: 1 });
    const flushing = other.sync.flush();
    other.sync.stop();
    await flushing;
    expect(other.timers.delays()).toEqual([]);
  });

  it("valor igual ao atual não gera PUT", async () => {
    const server = fakeUiStateServer({ rev: 1, data: { selection: { taskId: "T1" } } });
    const { sync, timers } = makeSync(server);
    await sync.start();
    expect(sync.set("selection", { taskId: "T1" })).toBe(true);
    expect(sync.hasPending()).toBe(false);
    expect(timers.delays()).toEqual([]);
  });

  it("adotar o envelope do servidor mantém a identidade das seções que não mudaram", async () => {
    const server = fakeUiStateServer({ rev: 1, data: { selection: { taskId: "T1" }, mobile: { tab: "plan" } } });
    const { sync, timers } = makeSync(server);
    await sync.start();
    const selection = sync.get("selection");
    sync.set("mobile", { tab: "board" });
    await timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(server.puts()).toHaveLength(1);
    expect(sync.get("selection")).toBe(selection);
    expect(sync.get("mobile")).toEqual({ tab: "board" });
  });

  it("cache local: primeira pintura com o cache; cache inválido é descartado", async () => {
    const storage = memoryStorage({ [uiStateCacheKey(PROJECT)]: JSON.stringify({ version: 1, rev: 9, data: { mobile: { tab: "plan" } } }) });
    const server = fakeUiStateServer({ rev: 9, data: { mobile: { tab: "plan" } } });
    const { sync } = makeSync(server, { storage });
    expect(sync.getSnapshot()).toMatchObject({ rev: 9, ready: false });
    expect(sync.get("mobile")).toEqual({ tab: "plan" });

    const broken = memoryStorage({ [uiStateCacheKey(PROJECT)]: "{nao-e-json" });
    const other = makeSync(server, { storage: broken }).sync;
    expect(other.getSnapshot()).toMatchObject({ rev: 0, data: {} });
    expect(broken.getItem(uiStateCacheKey(PROJECT))).toBeNull();
  });

  it("BroadcastChannel: a outra aba adota o rev novo e não sobrescreve", async () => {
    const server = fakeUiStateServer({ rev: 1 });
    const hub = memoryBroadcastHub();
    const a = makeSync(server, { createChannel: hub.create });
    const b = makeSync(server, { createChannel: hub.create });
    await a.sync.start();
    await b.sync.start();
    a.sync.set(CURRENT, { windows: { from: "a" } });
    await a.timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(b.sync.getSnapshot().rev).toBe(2);
    expect(b.sync.get(CURRENT)).toEqual({ windows: { from: "a" } });
    // A aba B grava sem 409, porque já conhece o rev.
    b.sync.set("selection", { taskId: "T9" });
    await b.timers.advance(UI_STATE_DEBOUNCE_MS);
    expect(server.puts().map((c) => (c.body as { base_rev: number }).base_rev)).toEqual([1, 2]);
    expect(server.state.data).toEqual({ layouts: { execution: { current: { windows: { from: "a" } } } }, selection: { taskId: "T9" } });
    expect(a.sync.get("selection")).toEqual({ taskId: "T9" });
  });

  it("mensagens de outro escopo, antigas ou malformadas são ignoradas", async () => {
    const server = fakeUiStateServer({ rev: 5 });
    const hub = memoryBroadcastHub();
    const { sync } = makeSync(server, { createChannel: hub.create });
    await sync.start();
    const other = hub.create("nants.ui-state");
    other.postMessage({ type: "ui-state", scope: "project.p2", sender: "x", rev: 99, data: { a: 1 } });
    other.postMessage({ type: "ui-state", scope: "project.p1", sender: "x", rev: 3, data: { a: 1 } });
    other.postMessage({ type: "ui-state", scope: "project.p1", sender: "x", rev: 8, data: "lixo" });
    expect(sync.getSnapshot()).toMatchObject({ rev: 5, data: {} });
    sync.stop();
  });
});

describe("request: opção keepalive (aditiva)", () => {
  it("repassa keepalive só quando pedida; os outros chamadores ficam iguais", async () => {
    const fetchImpl = vi.fn(async () => new Response("{}", { status: 200 }));
    await request({ method: "PUT", path: "/x", body: "{}", keepalive: true }, fetchImpl as unknown as typeof fetch);
    await request({ method: "PUT", path: "/x", body: "{}" }, fetchImpl as unknown as typeof fetch);
    const inits = fetchImpl.mock.calls.map((call) => (call as unknown as [string, RequestInit])[1]);
    expect(inits[0]?.keepalive).toBe(true);
    expect(inits[1] !== undefined && "keepalive" in inits[1]).toBe(false);
  });
});
