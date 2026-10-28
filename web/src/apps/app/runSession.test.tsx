/*
 * Fonte única do estado da execução (spec 095, T03, C-03): App e Execução leem a mesma sessão por
 * projeto e especificação.
 * - um único `GET .../app` por vez, pedido por qualquer janela visível (as duas abertas não
 *   dobram a leitura);
 * - registro consultado só com a Execução visível;
 * - um único aviso "App no ar" na transição sem endereço → com endereço, uma vez por partida;
 * - remontagem por `projectId:specId` nas duas janelas (respostas da sessão antiga descartadas) e
 *   a sessão descartada quando a última janela desmonta.
 */
import { act, cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { settle } from "../../desktop/shell-test-utils";
import { WindowLayoutContext } from "../../desktop/useWindowVisibility";
import { resetStore } from "../../test-utils";
import { ExecutionPanel } from "../execution/ExecutionApp";
import { AppRunner } from "./AppApp";
import {
  APP_URL,
  appEmpty,
  LOGS_URL,
  S,
  STATUS_URL,
  appWindow,
  calls,
  clickDock,
  emit,
  execWindow,
  frame,
  mountRun,
  running,
  startFromExecution,
} from "./app-test-utils";
import { LOGS_POLL_MS } from "./appModel";
import { liveSessionCount } from "./runSession";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const toasts = (): HTMLElement[] => [...document.querySelectorAll<HTMLElement>("[data-toast]")];
const liveToasts = (): HTMLElement[] => toasts().filter((t) => t.textContent?.includes("App no ar") === true);

describe("um único GET do estado por vez", () => {
  it("App e Execução abertos: uma leitura inicial só; um app_started relê uma vez", async () => {
    const view = await mountRun({ app: { status: running() }, open: ["App", "Execução"] });
    expect(calls(view, STATUS_URL)).toBe(1);
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(execWindow().querySelector("[data-app-state]")?.textContent).toBe("No ar");
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    expect(calls(view, STATUS_URL)).toBe(2);
  });

  it("pedidos que chegam com um GET em voo são agrupados num só pedido seguinte", async () => {
    const view = await mountRun({ app: { holdStatus: true }, open: ["App", "Execução"] });
    expect(view.app.held).toHaveLength(1);
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    // O segundo GET só sai depois do primeiro voltar.
    expect(view.app.held).toHaveLength(1);
    view.app.holdStatus = false;
    await act(async () => {
      view.app.held[0]?.();
    });
    await settle();
    expect(calls(view, STATUS_URL)).toBe(2);
  });

  it("a Execução visível lê o estado mesmo com o App minimizado (qualquer janela visível pede)", async () => {
    const view = await mountRun({ app: { status: running() }, open: [] });
    await clickDock("Execução");
    expect(calls(view, STATUS_URL)).toBe(1);
  });
});

describe("095/T07: subida sem passar por \"no ar sem endereço\"", () => {
  it("app_started antes da resposta do POST: o App segue \"subindo\" e mostra a página quando o POST traz as portas", async () => {
    const run = await mountRun({ app: { holdStart: true }, open: ["App", "Execução"] });
    await startFromExecution();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("starting");
    // O servidor publica o evento antes de responder ao POST (portas vazias no evento).
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    expect(appEmpty()?.getAttribute("data-app-empty")).toBe("starting");
    expect(appWindow().querySelector('[data-app-empty="no_address"]')).toBeNull();
    // A resposta do POST traz as portas: a página aparece, sem o estado vazio.
    await act(async () => {
      run.app.releaseStart?.();
    });
    await settle();
    expect(appEmpty()).toBeNull();
    expect(frame().getAttribute("src")).toBe(APP_URL);
  });
});

describe("registro só com a Execução visível", () => {
  it("só o App aberto, com o app no ar: nenhuma leitura do registro; a Execução visível lê e consulta a cada 2 s", async () => {
    const view = await mountRun({ app: { status: running() }, open: ["App"] });
    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LOGS_POLL_MS * 3);
    });
    expect(calls(view, LOGS_URL)).toBe(0);
    await clickDock("Execução");
    expect(calls(view, LOGS_URL)).toBe(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LOGS_POLL_MS + 10);
    });
    expect(calls(view, LOGS_URL)).toBe(2);
    // Minimizada, para; o App visível não lê o registro.
    await clickDock("Execução");
    const n = calls(view, LOGS_URL);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(LOGS_POLL_MS * 3);
    });
    expect(calls(view, LOGS_URL)).toBe(n);
    expect(appWindow().hidden).toBe(false);
  });
});

describe("aviso 'App no ar': um por partida", () => {
  it("a página aparece: um aviso só (com as duas janelas abertas); outra releitura não repete", async () => {
    const view = await mountRun({ open: ["App", "Execução"] });
    await startFromExecution();
    expect(liveToasts()).toHaveLength(1);
    const toast = liveToasts()[0];
    expect(toast?.getAttribute("role")).toBe("status");
    expect(toast?.querySelector(".nd-toast-title")?.textContent).toBe("App no ar");
    expect(toast?.querySelector(".nd-toast-body")?.textContent).toBe("A página foi aberta na janela App.");
    // Dispensado o aviso, o app_started da mesma partida e o Recarregar não avisam de novo.
    act(() => screen.getByRole("button", { name: "Fechar aviso" }).click());
    expect(toasts()).toHaveLength(0);
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    act(() => {
      within(within(appWindow()).getByRole("toolbar", { name: "Navegador do app" })).getByRole("button", { name: "Recarregar" }).click();
    });
    await settle();
    expect(toasts()).toHaveLength(0);
    expect(calls(view, STATUS_URL)).toBeGreaterThanOrEqual(2);
  });

  it("nova partida (parou e iniciou de novo): novo aviso", async () => {
    await mountRun({ open: ["App", "Execução"] });
    await startFromExecution();
    expect(liveToasts()).toHaveLength(1);
    act(() => screen.getByRole("button", { name: "Fechar aviso" }).click());
    expect(liveToasts()).toHaveLength(0);
    emit("app_exited", { code: 143, reason: "stopped" });
    await settle();
    await startFromExecution();
    expect(liveToasts()).toHaveLength(1);
  });

  it("carga inicial com o app já no ar: sem aviso", async () => {
    await mountRun({ app: { status: running() }, open: ["App", "Execução"] });
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(liveToasts()).toHaveLength(0);
  });

  it("partida de outra aba (app_started) com só o App aberto: avisa quando a página aparece", async () => {
    const view = await mountRun({ open: ["App"] });
    view.app.status = running();
    emit("app_started", { spec_id: S, url: null, ports: [], notices: [] });
    await settle();
    expect(frame().getAttribute("src")).toBe(APP_URL);
    expect(liveToasts()).toHaveLength(1);
  });

  it("a página aparece com o App fechado: o App abre com ela", async () => {
    await mountRun({ open: ["Execução"] });
    expect(screen.queryByRole("region", { name: "App" })).toBeNull();
    await startFromExecution();
    expect(liveToasts()).toHaveLength(1);
    expect(frame().getAttribute("src")).toBe(APP_URL);
  });
});

describe("remontagem por projeto e especificação", () => {
  it("as duas janelas trocam de sessão juntas; a resposta da antiga é descartada; sem janelas, nenhuma sessão", async () => {
    resetStore();
    const held: Array<{ url: string; release: () => void }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn((input: string | URL | Request) => {
        const url = String(input);
        if (url.endsWith("/logs")) return Promise.resolve(new Response("", { status: 200 }));
        return new Promise<Response>((resolve) => held.push({ url, release: () => resolve(new Response(JSON.stringify(running()), { status: 200 })) }));
      }),
    );
    const layout = {
      windows: {
        app: { x: 0, y: 0, w: 600, h: 400, z: 1, mode: "normal" as const, open: true },
        execution: { x: 600, y: 0, w: 400, h: 400, z: 2, mode: "normal" as const, open: true },
      },
    };
    const both = (specId: string) => (
      <WindowLayoutContext.Provider value={layout}>
        <AppRunner key={`app:p1:${specId}`} projectId="p1" specId={specId} size="wide" viewport="free" setSelection={() => undefined} />
        <ExecutionPanel key={`execution:p1:${specId}`} projectId="p1" specId={specId} projectPath="/x" size="wide" />
      </WindowLayoutContext.Provider>
    );
    const rendered = render(both("s1"));
    await settle();
    // Uma sessão, um GET para as duas janelas.
    // Spec 130: o "Como usar" e as provas (`/app/guide/<spec>`) são outra leitura.
    const statusUrls = (): string[] => held.map((h) => h.url).filter((u) => u.endsWith("/app"));
    expect(statusUrls()).toEqual(["/api/projects/p1/app"]);
    expect(liveSessionCount()).toBe(1);
    rendered.rerender(both("s2"));
    await settle();
    expect(statusUrls()).toEqual(["/api/projects/p1/app", "/api/projects/p1/app"]);
    expect(liveSessionCount()).toBe(1);
    // A resposta da sessão s1 chega depois da troca: descartada pelas duas janelas.
    const statusHeld = held.filter((h) => h.url.endsWith("/app"));
    await act(async () => {
      statusHeld[0]?.release();
    });
    await settle();
    expect(rendered.container.querySelector("iframe")).toBeNull();
    // Parado, com o guia ainda segurado: as duas janelas no estado neutro da spec 131 T05.
    expect(rendered.container.querySelectorAll('[data-runview="loading"]')).toHaveLength(2);
    expect(rendered.container.querySelector("[data-app-state]")).toBeNull();
    await act(async () => {
      statusHeld[1]?.release();
    });
    await settle();
    // A da s2 vale; a s2 não é a do app no ar (`running()` é da s1): outra especificação.
    expect(rendered.container.querySelector("[data-app-state]")?.getAttribute("data-app-state")).toBe("other");
    rendered.unmount();
    expect(liveSessionCount()).toBe(0);
  });
});
