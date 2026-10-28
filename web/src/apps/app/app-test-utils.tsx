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

/** "Como usar" e provas padrão (spec 130): roda aqui, com relatório e `run_command` declarado (131 T05). */
export function guide(extra: Record<string, unknown> = {}) {
  return {
    spec_id: S,
    how_to_use: { source: "report", markdown: "## Como executar\nRode `ledger report --month 2026-09`.", truncated: false },
    proofs: { tasks_done: 7, tasks_total: 7, final_verification: { passed: true, text: "6 de 6 critérios" }, captures: [], captures_total: 0 },
    sandbox: { runs_here: true, note: null },
    run: { declared: true, nothing_to_run: null },
    ...extra,
  };
}

/** Console vazio (spec 130). */
export const CONSOLE_EMPTY = { active: null, last: null, history: [], limits: { timeout_secs: 300, output_bytes: 262144 } };

interface AppServer {
  status: Record<string, unknown>;
  /** `GET .../app/guide/:spec` (spec 130). */
  guide: { status: number; body: unknown };
  /** `GET .../app/exec` (spec 130). */
  console: unknown;
  /** `POST .../app/exec` (spec 130). */
  exec: { status: number; body: unknown };
  /** `POST .../app/fix` (spec 130). */
  fix: { status: number; body: unknown };
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
    guide: { status: 200, body: guide() },
    console: CONSOLE_EMPTY,
    exec: { status: 202, body: { exec_id: "e1", spec_id: S, command: "ls", status: "running", started_at: "2026-10-27T12:00:00Z", timeout_secs: 300, origin: { device: null, loopback: true } } },
    fix: { status: 200, body: { reply: "Vou ajustar o Dockerfile.", draft_updated: true, turn_id: "t-fix" } },
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
      if (url.includes("/app/guide/")) return json(server.guide.body, server.guide.status);
      if (url.endsWith("/app/exec/stop")) return json(server.console);
      if (url.endsWith("/app/exec")) return method === "POST" ? json(server.exec.body, server.exec.status) : json(server.console);
      if (url.endsWith("/app/fix")) return json(server.fix.body, server.fix.status);
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
