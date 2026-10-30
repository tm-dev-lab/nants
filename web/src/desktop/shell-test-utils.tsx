/*
 * Servidor falso do Nants e montagem do shell para os testes da T05 em diante: config, estado dos
 * projetos, histórico, atividades, diálogo, uso, atenção, prontidão, ui-state (com `rev` e 409)
 * e as mutações usadas pela barra. Cada chamada fica registrada em `calls`.
 */
import { act, fireEvent, render, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { flushSync } from "react-dom";
import type { ChatHistoryTurn, ConnectionView, DialogMessage, ProjectState, ProjectEntry } from "../api/types";
import type { AppRegistry } from "../apps/registry";
import { FakeEventSource, installFakeEventSource, installFetch, makeConfig, resetStore, type RouteReply } from "../test-utils";
import type { Attention, Readiness } from "./api";
import { Shell } from "./Shell";
import type { ResizeObserverFactory } from "./useContainerSize";
import type { FrameScheduler } from "./useWindowGesture";

export interface ServerCall {
  method: string;
  url: string;
  body: unknown;
}

interface FakeServerOptions {
  projects?: ProjectEntry[];
  states?: Record<string, ProjectState>;
  attention?: Attention;
  readiness?: Readiness;
  connections?: ConnectionView[];
  /** ui-state por escopo ("global" ou o id do projeto). */
  ui?: Record<string, { rev: number; data: Record<string, unknown> }>;
  /** Diálogo persistido dos agentes (o mesmo para toda spec). */
  dialog?: DialogMessage[];
  /** Histórico do chat do Arquiteto por projeto. */
  chat?: Record<string, ChatHistoryTurn[]>;
}

export const READY: Readiness = {
  chat: { ok: true, missing: [] },
  create_project: { ok: true, missing: [] },
  execute: { ok: true, missing: [] },
  recommended: { ok: true, missing: [] },
};

export function notReady(): Readiness {
  const connection = { id: "connection", label: "Conexão com um provedor", settings_tab: "connections" as const };
  const architect = { id: "agent_architect", label: "Arquiteto", settings_tab: "agents" as const };
  const docker = { id: "docker_daemon", label: "Docker daemon", fix: "sudo systemctl start docker", settings_tab: "machine" as const };
  return {
    chat: { ok: false, missing: [connection, architect] },
    create_project: { ok: false, missing: [connection, architect] },
    execute: { ok: false, missing: [connection, architect, docker] },
    recommended: { ok: true, missing: [] },
  };
}

export const NO_ATTENTION: Attention = { setup_incomplete: false, projects: [] };

const json = (status: number, body: unknown): RouteReply => ({ status, body });

/** Servidor falso; `reply` permite trocar a resposta de uma rota (método + regex). */
export function fakeNantsServer(options: FakeServerOptions = {}) {
  const model = {
    projects: options.projects ?? [],
    states: { ...(options.states ?? {}) } as Record<string, ProjectState>,
    attention: options.attention ?? NO_ATTENTION,
    readiness: options.readiness ?? READY,
    connections: options.connections ?? [],
    ui: { ...(options.ui ?? {}) } as Record<string, { rev: number; data: Record<string, unknown> }>,
  };
  const calls: ServerCall[] = [];
  const overrides: { method: string; pattern: RegExp; reply: (body: unknown, url: string) => RouteReply }[] = [];

  const setPath = (data: Record<string, unknown>, section: string, value: unknown): Record<string, unknown> => {
    const copy = structuredClone(data);
    const parts = section.split(".");
    let node: Record<string, unknown> = copy;
    parts.forEach((part, i) => {
      if (i === parts.length - 1) node[part] = value;
      else {
        const child = node[part];
        node[part] = typeof child === "object" && child !== null && !Array.isArray(child) ? child : {};
        node = node[part] as Record<string, unknown>;
      }
    });
    return copy;
  };

  const uiScope = (url: string): string | null => {
    if (url === "/api/ui-state") return "global";
    const m = /^\/api\/projects\/([^/]+)\/ui-state$/.exec(url);
    return m?.[1] ?? null;
  };

  const handler = (method: string, url: string, body: unknown): RouteReply => {
    calls.push({ method, url, body });
    const override = overrides.find((o) => o.method === method && o.pattern.test(url));
    if (override !== undefined) return override.reply(body, url);
    if (method === "GET" && url === "/api/config") {
      return json(200, { ...makeConfig(model.projects), connections: model.connections, version: "0.1.0" });
    }
    if (method === "GET" && url === "/api/attention") return json(200, model.attention);
    if (method === "GET" && url === "/api/readiness") return json(200, model.readiness);
    if (method === "POST" && url === "/api/readiness/refresh") return json(200, model.readiness);
    const scope = uiScope(url);
    if (scope !== null) {
      const env = model.ui[scope] ?? { rev: 0, data: {} };
      model.ui[scope] = env;
      if (method === "GET") return json(200, { version: 1, ...env });
      const put = body as { base_rev: number; section: string; value: unknown };
      if (put.base_rev !== env.rev) return json(409, { version: 1, ...env, error: "ui_state_conflict" });
      model.ui[scope] = { rev: env.rev + 1, data: setPath(env.data, put.section, put.value) };
      return json(200, { version: 1, ...model.ui[scope] });
    }
    const project = /^\/api\/projects\/([^/]+)\/(state|chat|activity|usage)/.exec(url);
    if (project !== null && method === "GET") {
      const id = project[1] ?? "";
      const kind = project[2];
      const state = model.states[id];
      if (state === undefined) return json(404, { error: "projeto não encontrado" });
      if (kind === "state") return json(200, state);
      if (kind === "chat") return json(200, { turns: options.chat?.[id] ?? [] });
      if (kind === "activity") return json(200, []);
      return json(200, { specs: [], total: { calls: 0, input: 0, output: 0, cached: 0, reasoning: 0, cost_usd: null, unpriced_calls: 0 }, budget: null });
    }
    if (method === "GET" && /\/dialog/.test(url)) {
      const messages = options.dialog ?? [];
      return json(200, { messages, last_seq: messages.at(-1)?.seq ?? 0, truncated_before: false });
    }
    return json(404, { error: "rota não encontrada no servidor falso" });
  };

  return {
    model,
    calls,
    handler,
    reply(method: string, pattern: RegExp, reply: (body: unknown, url: string) => RouteReply) {
      overrides.unshift({ method, pattern, reply });
    },
    count(method: string, pattern: RegExp): number {
      return calls.filter((c) => c.method === method && pattern.test(c.url)).length;
    },
  };
}

/** Deixa as promessas e os timers de 0 ms correrem. */
export async function settle(rounds = 30): Promise<void> {
  await act(async () => {
    for (let i = 0; i < rounds; i += 1) await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

export const TEST_UI_DEPS = {
  storage: null,
  createChannel: null,
  setTimer: (fn: () => void) => setTimeout(fn, 0),
  clearTimer: (handle: unknown) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

export interface RenderShellOptions extends FakeServerOptions {
  url?: string;
  apps?: AppRegistry;
  renderAttentionPanel?: (props: { onClose: () => void }) => ReactNode;
  attentionIntervalMs?: number;
  /** Observador de tamanho das janelas (padrão: nenhum; o `data-size` fica "wide"). */
  createObserver?: ResizeObserverFactory | null;
  /** Agendador de quadros (padrão: o do navegador). */
  scheduler?: FrameScheduler;
}

/** Monta o shell com o servidor falso, o EventSource falso e a URL pedida. */
export async function renderShell(options: RenderShellOptions = {}) {
  resetStore();
  installFakeEventSource();
  const server = fakeNantsServer(options);
  const fetchMock = installFetch(server.handler);
  window.history.replaceState({}, "", options.url ?? "/");
  const view = render(
    <Shell
      uiStateDeps={TEST_UI_DEPS}
      attentionIntervalMs={options.attentionIntervalMs ?? 0}
      createObserver={options.createObserver ?? null}
      {...(options.scheduler !== undefined ? { scheduler: options.scheduler } : {})}
      {...(options.apps !== undefined ? { apps: options.apps } : {})}
      {...(options.renderAttentionPanel !== undefined ? { renderAttentionPanel: options.renderAttentionPanel } : {})}
    />,
  );
  await settle();
  const stream = (): FakeEventSource => {
    const source = FakeEventSource.instances[0];
    if (source === undefined) throw new Error("stream ausente");
    return source;
  };
  return { server, fetchMock, view, stream };
}

/**
 * Abre o menu do N (spec 137), se ainda não estiver aberto, e devolve o painel. Síncrono: serve
 * dentro de `act(async () => ...)`.
 */
export function openNMenu(): HTMLElement {
  const button = document.querySelector<HTMLButtonElement>("[data-nbutton]");
  if (button === null) throw new Error("botão N ausente");
  // `flushSync`: dentro de um `act` o React só desenha no fim; o menu precisa estar na tela agora.
  if (button.getAttribute("aria-expanded") !== "true") flushSync(() => fireEvent.click(button));
  const panel = document.querySelector<HTMLElement>("[data-nmenu]");
  if (panel === null) throw new Error("menu do N ausente");
  return panel;
}

/**
 * Ativa um app pelo menu do N, como o clique no item da grade: abre, foca, restaura ou minimiza a
 * janela que está em foco. Síncrono, como o `fireEvent`.
 */
export function activateApp(name: string | RegExp): void {
  const panel = openNMenu();
  const grid = panel.querySelector<HTMLElement>("[data-nmenu-grid]");
  if (grid === null) throw new Error("grade do menu do N ausente");
  fireEvent.click(within(grid).getByRole("button", { name }));
}

/** Roda `fn` com o menu do N aberto e o fecha depois (Esc), se foi aberto aqui. */
export function inNMenu<T>(fn: (menu: HTMLElement) => T): T {
  const wasOpen = document.querySelector("[data-nbutton]")?.getAttribute("aria-expanded") === "true";
  const out = fn(openNMenu());
  if (!wasOpen) flushSync(() => fireEvent.keyDown(window, { key: "Escape" }));
  return out;
}

/** Selo do app no menu do N (`null` sem selo). */
export function appBadge(kind: string): string | null {
  return inNMenu((menu) => menu.querySelector(`.nd-nmenu-tile[data-kind="${kind}"] [data-badge]`)?.textContent ?? null);
}

/** Abre pelo menu do N, um a um, todos os apps fechados da grade. */
export async function openAllApps(): Promise<void> {
  // Teto: um app que não abre não prende o teste num laço.
  for (let round = 0; round < 20; round += 1) {
    const kind = inNMenu((menu) => menu.querySelector('.nd-nmenu-tile[data-state="closed"]')?.getAttribute("data-kind") ?? null);
    if (kind === null) break;
    await act(async () => {
      const tile = openNMenu().querySelector<HTMLElement>(`.nd-nmenu-tile[data-kind="${kind}"]`);
      if (tile !== null) fireEvent.click(tile);
    });
  }
  await settle();
}
