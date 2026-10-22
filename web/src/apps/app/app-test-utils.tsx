/*
 * Utilitários dos testes das janelas App e Execução (spec 095, T03): o shell montado com um
 * servidor falso do app (`/api/projects/p1/app`, `start`, `stop`, `logs`), as respostas prontas e
 * os atalhos para achar as janelas e confirmar o início.
 */
import { act, fireEvent, screen, within } from "@testing-library/react";
import type { ProjectState, SpecStatus } from "../../api/types";
import { parseApiEvent } from "../../api/events";
import { renderShell, settle, type RenderShellOptions } from "../../desktop/shell-test-utils";
import { appStore } from "../../store";
import { makeProject, makeProjectState, makeSpec, makeTask } from "../../test-utils";

const P1 = makeProject("p1", "Loja");
export const S = "s1";
// O jsdom serve a página em http://localhost:3000: o link do app usa o mesmo host, outra porta.
export const HOST = window.location.hostname;
export const APP_URL = `http://${HOST}:18080`;

function projectState(status: SpecStatus = "COMPLETED"): ProjectState {
  return makeProjectState({ id: "p1", name: "Loja", specs: [makeSpec(S, "Loja online", status, [makeTask("T01", "Base", "DONE")])] });
}

export function running(extra: Record<string, unknown> = {}) {
  return {
    state: "running",
    mode: "docker",
    spec_id: S,
    url: null,
    started_at: "x",
    exit_code: null,
    ports: [{ service: "web", target: 8080, published: 18080, url: APP_URL, embeddable: true }],
    notices: [],
    embeddable: true,
    ...extra,
  };
}

export const STOPPED = { state: "stopped", spec_id: null, url: null, started_at: null, exit_code: null };

interface AppServer {
  status: Record<string, unknown>;
  start: { status: number; body: unknown };
  stop: { status: number; body: unknown };
  logs: string;
  /** Respostas do GET de estado seguradas até `release` (descarte de respostas atrasadas). */
  holdStatus: boolean;
  held: Array<() => void>;
  /** POST de início segurado até `releaseStart` (o App fica em "subindo"). */
  holdStart: boolean;
  releaseStart: (() => void) | null;
}

/** Layout do "Concluído" só com a Conversa: App e Execução são abertos pelo dock. */
const DONE_ONLY_CONVERSATION = {
  layouts: { done: { current: { windows: { conversation: { x: 0, y: 0, w: 500, h: 400, z: 1, mode: "normal", open: true } } }, mine: null } },
};

type RunWindow = "App" | "Execução";

interface MountOptions {
  app?: Partial<AppServer>;
  status?: SpecStatus;
  ui?: Record<string, unknown>;
  /** Estado da interface global (ex.: a seção `network` da 097). */
  globalUi?: Record<string, unknown>;
  /** Janelas abertas pelo dock depois das rotas falsas (padrão: só o App). */
  open?: readonly RunWindow[];
  extra?: Partial<RenderShellOptions>;
}

/** Abre (ou foca, restaura, minimiza) a janela pelo dock. */
export async function clickDock(name: RunWindow): Promise<void> {
  await act(async () => {
    fireEvent.click(within(screen.getByRole("navigation", { name: "Dock" })).getByRole("button", { name }));
  });
  await settle();
}

export async function mountRun({ app = {}, status = "COMPLETED", ui = DONE_ONLY_CONVERSATION, globalUi, open = ["App"], extra = {} }: MountOptions = {}) {
  const server: AppServer = {
    status: STOPPED,
    start: { status: 200, body: running() },
    stop: { status: 200, body: { state: "stopped" } },
    logs: "listening",
    holdStatus: false,
    held: [],
    holdStart: false,
    releaseStart: null,
    ...app,
  };
  const uiScopes = { p1: { rev: 1, data: ui }, ...(globalUi !== undefined ? { global: { rev: 1, data: globalUi } } : {}) };
  const view = await renderShell({ url: "/?space=p1", projects: [P1], states: { p1: projectState(status) }, ui: uiScopes, ...extra });
  const base = view.fetchMock.getMockImplementation();
  view.fetchMock.mockImplementation(async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = init?.method ?? "GET";
    if (/\/api\/projects\/p1\/app/.test(url)) {
      const body: unknown = typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : undefined;
      view.server.calls.push({ method, url, body });
      const json = (data: unknown, code = 200) => new Response(JSON.stringify(data), { status: code, headers: { "Content-Type": "application/json" } });
      if (url.endsWith("/app/logs")) return new Response(server.logs, { status: 200 });
      if (url.endsWith("/app/start")) {
        if (!server.holdStart) return json(server.start.body, server.start.status);
        return new Promise<Response>((resolve) => {
          server.releaseStart = () => resolve(json(server.start.body, server.start.status));
        });
      }
      if (url.endsWith("/app/stop")) return json(server.stop.body, server.stop.status);
      if (server.holdStatus) {
        const snapshot = server.status;
        return new Promise<Response>((resolve) => server.held.push(() => resolve(json(snapshot))));
      }
      return json(server.status);
    }
    if (base === undefined) throw new Error("fetch ausente");
    return base(input, init);
  });
  for (const name of open) await clickDock(name);
  return { ...view, app: server };
}

type Mounted = Awaited<ReturnType<typeof mountRun>>;

export const STATUS_URL = /\/api\/projects\/p1\/app$/;
export const START_URL = /\/app\/start$/;
export const STOP_URL = /\/app\/stop$/;
export const LOGS_URL = /\/app\/logs$/;

export const calls = (view: Mounted, suffix: RegExp): number => view.server.calls.filter((c) => suffix.test(c.url)).length;

export function appWindow(): HTMLElement {
  return screen.getByRole("region", { name: "App" });
}

export function execWindow(): HTMLElement {
  return screen.getByRole("region", { name: "Execução" });
}

export function emit(type: string, payload: Record<string, unknown>): void {
  const event = parseApiEvent({ event_type: type, project_id: "p1", payload });
  if (event === null) throw new Error("evento inválido");
  act(() => appStore.applyEvent(event));
}

/** Confirma o diálogo de início já aberto ("Executar"). */
export async function confirmDialog(): Promise<void> {
  const dialog = screen.getByRole("dialog", { name: "Iniciar o app?" });
  await act(async () => {
    fireEvent.click(within(dialog).getByRole("button", { name: "Executar" }));
  });
  await settle();
}

/** "Iniciar" na Execução e "Executar" no diálogo. */
export async function startFromExecution(): Promise<void> {
  await act(async () => {
    fireEvent.click(within(execWindow()).getByRole("button", { name: "Iniciar" }));
  });
  await confirmDialog();
}

export function frame(): HTMLIFrameElement {
  const el = appWindow().querySelector<HTMLIFrameElement>("iframe[data-app-frame]");
  if (el === null) throw new Error("iframe ausente");
  return el;
}

/** Estado vazio mostrado no App (`data-app-empty`), ou `null`. */
export function appEmpty(): HTMLElement | null {
  return appWindow().querySelector<HTMLElement>("[data-app-empty]");
}
