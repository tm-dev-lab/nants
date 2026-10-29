/*
 * Releitura da lista da base (spec 134 T06, F-01): um `kb_updated` que chega com um GET em voo não
 * se perde: o GET se repete enquanto houver marca pendente, e a última resposta vale.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyKbEvent, kbStateOf, reloadKb, resetKbStore } from "./kbStore";

interface Pending {
  resolve: (body: unknown) => void;
}

function deferredFetch() {
  const queue: Pending[] = [];
  const mock = vi.fn(
    () =>
      new Promise<Response>((done) => {
        queue.push({ resolve: (body) => done(new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } })) });
      }),
  );
  vi.stubGlobal("fetch", mock);
  return { mock, queue };
}

const list = (status: string) => ({ docs: [{ id: "a-3f9a1c2e", title: "A", kind: "pdf", source: "file", status }], has_overview: false });

async function flush(): Promise<void> {
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
  await new Promise((r) => setTimeout(r, 0));
}

beforeEach(() => resetKbStore());
afterEach(() => vi.unstubAllGlobals());

describe("kbStore: evento durante o GET em voo", () => {
  it("o evento liga a marca pendente: sai mais um GET e o estado final é o do servidor", async () => {
    const { mock, queue } = deferredFetch();
    const first = reloadKb("p1");
    queue.shift()?.resolve(list("pending"));
    await first;
    expect(statusOf()).toBe("pending");

    const calls = mock.mock.calls.length;
    applyKbEvent("p1"); // começa o GET
    await flush();
    applyKbEvent("p1"); // chega durante o GET: só liga a marca
    await flush();
    expect(mock.mock.calls.length - calls).toBe(1);
    // Cada resposta é gravada; o GET se repete porque havia marca.
    queue.shift()?.resolve(list("extracting"));
    await flush();
    expect(statusOf()).toBe("extracting");
    expect(mock.mock.calls.length - calls).toBe(2);
    queue.shift()?.resolve(list("ready"));
    await flush();
    expect(statusOf()).toBe("ready");
    expect(mock.mock.calls.length - calls).toBe(2);
  });

  it("um evento durante o segundo GET gera um terceiro; vale a última resposta", async () => {
    const { mock, queue } = deferredFetch();
    const first = reloadKb("p1");
    queue.shift()?.resolve(list("pending"));
    await first;
    const calls = mock.mock.calls.length;
    applyKbEvent("p1");
    await flush();
    applyKbEvent("p1"); // durante o primeiro GET
    await flush();
    queue.shift()?.resolve(list("pending"));
    await flush();
    expect(mock.mock.calls.length - calls).toBe(2);
    applyKbEvent("p1"); // durante o segundo GET
    await flush();
    queue.shift()?.resolve(list("extracting"));
    await flush();
    expect(mock.mock.calls.length - calls).toBe(3);
    queue.shift()?.resolve(list("ready"));
    await flush();
    expect(statusOf()).toBe("ready");
    expect(mock.mock.calls.length - calls).toBe(3);
    // Sem marca, o laço para: nenhum GET a mais.
    await flush();
    expect(mock.mock.calls.length - calls).toBe(3);
  });

  it("sem evento no meio, um GET só", async () => {
    const { mock, queue } = deferredFetch();
    const once = reloadKb("p1");
    queue.shift()?.resolve(list("ready"));
    await once;
    expect(mock.mock.calls.length).toBe(1);
  });
});

function statusOf(): string | undefined {
  const state = kbStateOf("p1");
  return state?.status === "ok" ? state.list.docs[0]?.status : undefined;
}
