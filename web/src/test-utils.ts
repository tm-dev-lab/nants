/*
 * Utilitários de teste: `fetch` e `EventSource` mockados (sem rede), fixtures de contrato
 * reset do store global e `renderWith({ lang })`. Usado só pelos testes (*.test.tsx).
 */
import type { ReactElement } from "react";
import { render, type RenderOptions, type RenderResult } from "@testing-library/react";
import { vi } from "vitest";
import { setLanguage, translate, type Lang } from "./i18n";
import { ptBR } from "./i18n/ptBR";
import { MAX_CHAT_ATTACHMENTS } from "./chatImages";
import type { AppState } from "./store";
import { appStore } from "./store";
import type {
  ConfigView,
  ProjectState,
  ProjectEntry,
  SpecStatus,
  SpecView,
  TaskStatus,
  TaskView,
} from "./api/types";

/** Subconjunto de EventSource: registra listeners e permite emitir eventos nomeados. */
export class FakeEventSource {
  static instances: FakeEventSource[] = [];
  readonly url: string;
  readyState = 0;
  onopen: ((ev: Event) => void) | null = null;
  onerror: ((ev: Event) => void) | null = null;
  private readonly listeners = new Map<string, ((ev: MessageEvent<string>) => void)[]>();

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: (ev: MessageEvent<string>) => void): void {
    const list = this.listeners.get(type) ?? [];
    list.push(listener);
    this.listeners.set(type, list);
  }

  close(): void {
    this.readyState = 2;
  }

  /** Dispara um evento nomeado com `data` serializado como o servidor faz. */
  emit(type: string, data: unknown): void {
    const event = new MessageEvent<string>(type, { data: JSON.stringify(data) });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

/** Substitui `EventSource` global por `FakeEventSource`. */
export function installFakeEventSource(): void {
  FakeEventSource.instances = [];
  vi.stubGlobal("EventSource", FakeEventSource);
}

export interface RouteReply {
  status?: number;
  body?: unknown;
}

type FetchHandler = (method: string, url: string, body: unknown) => RouteReply | Promise<RouteReply>;

/** Instala `fetch` global que responde pelo `handler`. Retorna o mock para inspeção. */
export function installFetch(handler: FetchHandler) {
  const mock = vi.fn(async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = init?.method ?? "GET";
    const body: unknown = typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : undefined;
    const reply = await handler(method, url, body);
    return new Response(reply.body === undefined ? null : JSON.stringify(reply.body), {
      status: reply.status ?? 200,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", mock);
  return mock;
}

/** Restaura o store global para um estado limpo (mais os campos informados). */
export function resetStore(initial: Partial<AppState> = {}): void {
  appStore.setState({
    config: null,
    configError: null,
    projects: [],
    activeProjectId: null,
    projectStates: {},
    messages: {},
    permissions: {},
    resolvedPermissions: {},
    phases: {},
    revisions: {},
    liveRevisions: {},
    reportSignals: {},
    appSignals: {},
    recoveries: {},
    blockKinds: {},
    shownTurns: {},
    removedProjectIds: {},
    ...initial,
  });
}

export function makeConfig(projects: ProjectEntry[]): ConfigView {
  const agent = { connection_id: "conn", model: "model", effort: "low" as const };
  return {
    server: { host: "127.0.0.1", port: 3000 },
    connections: [],
    agents: { architect: agent, developer: agent, reviewer: agent },
    projects,
  };
}

export function makeProject(id: string, name: string): ProjectEntry {
  return { id, name, path: `/home/dev/${name}`, created_at: "2026-10-14T00:00:00Z" };
}

export function makeProjectState(overrides: Partial<ProjectState> = {}): ProjectState {
  return {
    id: "p1",
    name: "api-pagamentos",
    is_git_repo: true,
    initialized: true,
    base_branch: "main",
    test_command: "make test",
    test_command_redacted: false,
    max_attempts: 5,
    max_test_cycles: 5,
    running: false,
    run: null,
    specs: [],
    pending_permissions: [],
    ...overrides,
  };
}

export function makeTask(id: string, title: string, status: TaskStatus | null, extra: Partial<TaskView> = {}): TaskView {
  return {
    id,
    title,
    status,
    attempts: 0,
    max_attempts: 3,
    depends_on: [],
    last_review_verdict: null,
    ...extra,
  };
}

export function makeSpec(id: string, title: string, status: SpecStatus | null, tasks: TaskView[] = []): SpecView {
  return { id, title, status, tasks };
}

interface RenderWithOptions extends RenderOptions {
  /** Idioma ativo durante o teste (padrão pt-BR, o mesmo do `test-setup.ts`). */
  lang?: Lang;
}

/**
 * `render` com o idioma escolhido (spec 101, C-05): troca o idioma ativo antes de renderizar, sem
 * gravar o espelho local. O `test-setup.ts` volta a pt-BR antes do próximo teste.
 */
export function renderWith(ui: ReactElement, { lang = "pt-BR", ...options }: RenderWithOptions = {}): RenderResult {
  setLanguage(lang, { persist: false });
  return render(ui, options);
}

/*
 * Textos dos anexos da Conversa em pt-BR, o idioma padrão dos testes (spec 115 T04, C-11): a tela
 * usa as chaves `attachments.*` no idioma ativo; as constantes ficam só aqui.
 */
export const ATTACH_LABEL = ptBR["attachments.attach"];
export const ATTACHMENTS_LIMIT_NOTICE = translate("pt-BR", "attachments.limit", { max: MAX_CHAT_ATTACHMENTS });
export const ATTACHMENTS_FAILED_NOTICE = ptBR["attachments.failed"];
export const ATTACHMENTS_PREVIEW_LABEL = ptBR["attachments.previewLabel"];
export const USER_IMAGES_LABEL = ptBR["attachments.sentLabel"];
export const THUMB_PLACEHOLDER_TEXT = ptBR["attachments.placeholder"];
export const ATTACHED_IMAGE_TITLE = ptBR["attachments.attachedTitle"];
