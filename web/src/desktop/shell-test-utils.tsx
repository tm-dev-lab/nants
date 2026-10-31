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
import { resetNantyStore } from "../apps/nanty/nantyStore";
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
  /**
   * Nanty (spec 138 T04 e T10): `GET /api/nanty/status`, `/api/nanty/feed`, `/api/nanty/conversation`
   * e `/api/nanty/usage` (padrão: atento, sem cartões, sem mensagens e sem gasto).
   */
  nanty?: { status?: unknown; feed?: unknown; conversation?: unknown; usage?: unknown; config?: unknown; telegram?: unknown };
}

export const NANTY_STATUS = {
  instance: "active",
  avatar: "attentive",
  model_configured: false,
  telegram_configured: false,
  telegram_paired: false,
};

export const NANTY_EMPTY_FEED = {
  waiting: [],
  done: [],
  planned: [],
  today: { day: "2026-10-30", spend: { day: "2026-10-30", usd: 0, input_tokens: 0, output_tokens: 0, unpriced_turns: 0 }, budget_usd: 0.5, waiting: 0, done: 0 },
};

/** `GET /api/nanty/config` (T11): os valores iniciais, sem modelo e sem Telegram. Só com `nanty.config`. */
export const NANTY_CONFIG = {
  now: "09:41",
  connection_id: "",
  model: "",
  effort: "low",
  daily_budget_usd: 0.5,
  daily_budget_tokens: 200000,
  quiet_start: "22:00",
  quiet_end: "07:00",
  digest_time: "08:00",
  digest_empty: "short",
  lang: "pt-BR",
  excluded_projects: [],
  actions: {},
  catalog: [
    { id: "approve_plan", kind: "executes", level: "ask", default_level: "ask" },
    { id: "resume_run", kind: "executes", level: "ask", default_level: "ask" },
    { id: "retry_provider", kind: "executes", level: "auto", default_level: "auto" },
    { id: "swap_reserve", kind: "executes", level: "auto", default_level: "auto" },
    { id: "clean_orphans", kind: "executes", level: "auto", default_level: "auto" },
    { id: "pause_run", kind: "executes", level: "ask", default_level: "ask" },
    { id: "allow_command_always", kind: "person_only", level: "ask", default_level: "ask" },
    { id: "change_channel", kind: "person_only", level: "ask", default_level: "ask" },
  ],
  reserve: {},
  voice: { enabled: false, order: [], transcribe: { connection_id: "", model: "" }, max_input_secs: 300 },
  telegram: { bot_token_masked: "", paired: false, hold_while_visible: true, visible_hold_secs: 180 },
};

/** `GET /api/nanty/telegram` (T11): sem bot. Só com `nanty.telegram`. */
export const NANTY_TELEGRAM = { health: "not_configured", bot_token_masked: "", configured: false, paired: false, pairing: { state: "closed" } };

const NANTY_ZERO_SPEND = { day: "2026-10-30", usd: 0, input_tokens: 0, output_tokens: 0, unpriced_turns: 0, voice_calls: 0 };

export const NANTY_EMPTY_USAGE = {
  budget: { mode: "usd", day: "2026-10-30", spent_usd: 0, budget_usd: 0.5, tokens: 0, budget_tokens: 200000, unpriced_turns: 0, unpriced: false, plan: false, quota_shared: false, exhausted: false },
  today: NANTY_ZERO_SPEND,
  history: [],
};

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
    nantyStatus: options.nanty?.status ?? NANTY_STATUS,
    nantyFeed: options.nanty?.feed ?? NANTY_EMPTY_FEED,
    nantyConversation: options.nanty?.conversation ?? { messages: [] },
    nantyUsage: options.nanty?.usage ?? NANTY_EMPTY_USAGE,
    nantyConfig: options.nanty?.config,
    nantyTelegram: options.nanty?.telegram,
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
    if (method === "GET" && url === "/api/nanty/status") return json(200, model.nantyStatus);
    if (method === "GET" && url === "/api/nanty/feed") return json(200, model.nantyFeed);
    if (method === "GET" && url === "/api/nanty/conversation") return json(200, model.nantyConversation);
    if (method === "GET" && url === "/api/nanty/usage") return json(200, model.nantyUsage);
    if (method === "GET" && url === "/api/nanty/config" && model.nantyConfig !== undefined) return json(200, model.nantyConfig);
    if (method === "GET" && url === "/api/nanty/telegram" && model.nantyTelegram !== undefined) return json(200, model.nantyTelegram);
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
  resetNantyStore();
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
