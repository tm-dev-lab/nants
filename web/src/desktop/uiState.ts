/*
 * Estado da UI salvo no servidor (spec 093, decisão 6; contrato em api-notes.md):
 * envelope `{version: 1, rev, data}`, PUT por seção com `base_rev` e 409 em conflito.
 *
 * `UiStateSync` é o cliente de UM escopo (global ou um projeto):
 * - grava com debounce de no máximo 1 PUT por segundo, uma seção por PUT;
 * - em 409, adota o envelope atual do servidor e reaplica só as próprias seções pendentes;
 * - no `pagehide`, manda `fetch keepalive` só com as seções alteradas;
 * - avisa as outras abas por `BroadcastChannel` (o envelope novo, com `rev`), para que duas abas
 *   não se sobrescrevam;
 * - o `localStorage` é só cache para a primeira pintura;
 * - envelope inválido ou de outra versão vira o padrão (`data` vazio), sem erro.
 */
import { request } from "../api/client";
import { defaultStorage, type StorageLike } from "./browserStorage";
import { LAYOUT_CATEGORIES, type LayoutCategory } from "./presets";
import { isSettingsWindowTab, type SettingsWindowTab } from "./api";

const UI_STATE_VERSION = 1;
/** Intervalo mínimo entre PUTs (≤ 1/s). */
export const UI_STATE_DEBOUNCE_MS = 1_000;
/** Nova tentativa depois de falha de rede ou 5xx. */
export const UI_STATE_RETRY_MS = 5_000;
/** Tentativas seguidas de reaplicar uma seção depois de 409. */
const MAX_CONFLICT_RETRIES = 3;
const CACHE_PREFIX = "nants.ui-state.";
const CHANNEL_NAME = "nants.ui-state";

export type UiScope = { kind: "global" } | { kind: "project"; projectId: string };

interface UiStateEnvelope {
  version: number;
  rev: number;
  data: Record<string, unknown>;
}

type LayoutSlot = "current" | "mine";

/** Seções do estado da UI. */
export const UI_SECTIONS = {
  appearance: "appearance",
  hints: "hints",
  /** Acesso pela rede (spec 097): `{released: {start, end} | null}`, por servidor. */
  network: "network",
  /** Idioma da interface (spec 101): `{lang: "pt-BR" | "en"}`, por servidor. */
  language: "language",
  /** Plano ChatGPT (spec 116 T03): `{welcomed: true}` depois das boas-vindas, que aparecem uma vez só. */
  chatgpt: "chatgpt",
  selection: "selection",
  mobile: "mobile",
  layout: (category: LayoutCategory, slot: LayoutSlot): string => `layouts.${category}.${slot}`,
} as const;

/** Seções que cada escopo aceita (o servidor aceita qualquer nome válido; o cliente restringe). */
export function isSectionAllowed(scope: UiScope, section: string): boolean {
  if (scope.kind === "global") {
    return (
      section === UI_SECTIONS.appearance ||
      section === UI_SECTIONS.hints ||
      section === UI_SECTIONS.network ||
      section === UI_SECTIONS.language ||
      section === UI_SECTIONS.chatgpt
    );
  }
  if (section === UI_SECTIONS.selection || section === UI_SECTIONS.mobile) return true;
  return LAYOUT_CATEGORIES.some((c) => section === UI_SECTIONS.layout(c, "current") || section === UI_SECTIONS.layout(c, "mine"));
}

function uiStateUrl(scope: UiScope): string {
  return scope.kind === "global" ? "/api/ui-state" : `/api/projects/${encodeURIComponent(scope.projectId)}/ui-state`;
}

export function scopeKey(scope: UiScope): string {
  return scope.kind === "global" ? "global" : `project.${scope.projectId}`;
}

// --- Caminhos seguros em `data` (sem __proto__ nem mescla cega) ---

const FORBIDDEN_PARTS: ReadonlySet<string> = new Set(["__proto__", "constructor", "prototype"]);

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return false;
  const proto: unknown = Object.getPrototypeOf(value);
  return proto === Object.prototype || proto === null;
}

function splitSection(section: string): string[] | null {
  const parts = section.split(".");
  return parts.every((p) => p.length > 0 && !FORBIDDEN_PARTS.has(p)) ? parts : null;
}

/** Valor da seção (caminho com pontos), ou `undefined`. */
export function getSection(data: Record<string, unknown>, section: string): unknown {
  const parts = splitSection(section);
  if (parts === null) return undefined;
  let node: unknown = data;
  for (const part of parts) {
    if (!isPlainObject(node) || !Object.hasOwn(node, part)) return undefined;
    node = node[part];
  }
  return node;
}

/** Cópia de `data` com a seção trocada; as irmãs são preservadas. */
export function setSection(data: Record<string, unknown>, section: string, value: unknown): Record<string, unknown> {
  const parts = splitSection(section);
  if (parts === null) return data;
  const write = (node: Record<string, unknown>, index: number): Record<string, unknown> => {
    const part = parts[index];
    if (part === undefined) return node;
    const copy: Record<string, unknown> = { ...node };
    if (index === parts.length - 1) {
      Object.defineProperty(copy, part, { value, enumerable: true, writable: true, configurable: true });
      return copy;
    }
    const child = Object.hasOwn(node, part) ? node[part] : undefined;
    const next = write(isPlainObject(child) ? child : {}, index + 1);
    Object.defineProperty(copy, part, { value: next, enumerable: true, writable: true, configurable: true });
    return copy;
  };
  return write(data, 0);
}

/** Igualdade estrutural por JSON (valores do ui-state são JSON). */
export function sameJson(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  try {
    return JSON.stringify(a) === JSON.stringify(b);
  } catch {
    return false;
  }
}

/**
 * `next` reaproveitando os nós de `prev` estruturalmente iguais (structural sharing): o que não
 * mudou mantém a identidade.
 */
function shareStructure<T>(prev: unknown, next: T): T {
  if (sameJson(prev, next)) return prev as T;
  if (!isPlainObject(prev) || !isPlainObject(next)) return next;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(next)) {
    const value = Object.hasOwn(prev, key) ? shareStructure(prev[key], next[key]) : next[key];
    Object.defineProperty(out, key, { value, enumerable: true, writable: true, configurable: true });
  }
  return out as T;
}

/** Envelope válido; o resto (inclusive outra versão) vira o padrão sem erro. */
export function parseEnvelope(value: unknown): UiStateEnvelope {
  const empty: UiStateEnvelope = { version: UI_STATE_VERSION, rev: 0, data: {} };
  if (!isPlainObject(value)) return empty;
  const rev = typeof value.rev === "number" && Number.isInteger(value.rev) && value.rev >= 0 ? value.rev : 0;
  if (value.version !== UI_STATE_VERSION || !isPlainObject(value.data)) return { ...empty, rev };
  return { version: UI_STATE_VERSION, rev, data: value.data };
}

// --- Seleção (tarefa em foco, arquivo aberto, rolagem da Conversa) ---

export interface UiSelection {
  taskId: string | null;
  filePath: string | null;
  conversationScroll: number | null;
  /** Colunas opcionais visíveis no Quadro (T07); ausente = padrão. */
  boardColumns?: readonly string[];
  /** Aba da janela Atividade (T07); ausente = "ao vivo". */
  activityTab?: "live" | "history";
  /** Fonte do arquivo aberto nos Arquivos (T08): `filePath` vale para ela; ausente = "base". */
  fileSource?: "base" | "task";
  /** Fração da largura (ou altura) da árvore nos Arquivos (T08), de 0,15 a 0,85. */
  filesSplit?: number;
  /** Aba ativa da janela Configurações (T08). */
  settingsTab?: SettingsWindowTab;
  /** Tamanho da tela do app embutido na janela App (T09); ausente = "livre". */
  appViewport?: AppViewport;
}

/** Tamanhos da tela do app embutido: livre (a janela inteira), 1440, 768 e 390 px de largura. */
export type AppViewport = "free" | "desktop" | "tablet" | "phone";
const APP_VIEWPORT_IDS: readonly AppViewport[] = ["free", "desktop", "tablet", "phone"];

function isAppViewport(value: unknown): value is AppViewport {
  return typeof value === "string" && (APP_VIEWPORT_IDS as readonly string[]).includes(value);
}

/** Limites da divisão árvore/visualizador dos Arquivos. */
export const FILES_SPLIT_MIN = 0.15;
export const FILES_SPLIT_MAX = 0.85;

/** Teto de colunas guardadas (os estados do fluxo cabem com folga). */
const MAX_BOARD_COLUMNS = 16;

export function parseSelection(value: unknown): UiSelection {
  const v = isPlainObject(value) ? value : {};
  const str = (x: unknown): string | null => (typeof x === "string" && x.length > 0 && x.length <= 1024 ? x : null);
  const scroll =
    typeof v.conversationScroll === "number" && Number.isFinite(v.conversationScroll) && v.conversationScroll >= 0
      ? v.conversationScroll
      : null;
  const out: UiSelection = { taskId: str(v.taskId), filePath: str(v.filePath), conversationScroll: scroll };
  if (Array.isArray(v.boardColumns)) {
    const columns = v.boardColumns.filter((c): c is string => typeof c === "string" && c.length > 0 && c.length <= 64);
    out.boardColumns = [...new Set(columns)].slice(0, MAX_BOARD_COLUMNS);
  }
  if (v.activityTab === "live" || v.activityTab === "history") out.activityTab = v.activityTab;
  if (v.fileSource === "base" || v.fileSource === "task") out.fileSource = v.fileSource;
  if (typeof v.filesSplit === "number" && Number.isFinite(v.filesSplit)) {
    out.filesSplit = Math.min(FILES_SPLIT_MAX, Math.max(FILES_SPLIT_MIN, v.filesSplit));
  }
  if (isSettingsWindowTab(v.settingsTab)) out.settingsTab = v.settingsTab;
  if (isAppViewport(v.appViewport)) out.appViewport = v.appViewport;
  return out;
}

// --- Cliente de um escopo ---

export interface UiStateSnapshot {
  rev: number;
  data: Record<string, unknown>;
  /** Já houve resposta do servidor (o cache pode ter preenchido antes). */
  ready: boolean;
}

export interface BroadcastLike {
  postMessage(message: unknown): void;
  close(): void;
  onmessage: ((event: MessageEvent) => void) | null;
}


export interface UiStateDeps {
  fetchImpl?: typeof fetch;
  /** `null` desliga o cache. */
  storage?: StorageLike | null;
  /** `null` desliga a sincronização entre abas. */
  createChannel?: ((name: string) => BroadcastLike) | null;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  /** Celular: só escreve na seção `mobile`. */
  mobileOnly?: boolean;
}

interface ChannelMessage {
  type: "ui-state";
  scope: string;
  sender: string;
  rev: number;
  data: Record<string, unknown>;
}

function defaultChannel(): ((name: string) => BroadcastLike) | null {
  return typeof BroadcastChannel === "function" ? (name) => new BroadcastChannel(name) : null;
}

let senderSeq = 0;

/** Id desta instância no canal (para ignorar o próprio eco). */
function newSenderId(): string {
  senderSeq += 1;
  const random = typeof crypto !== "undefined" && typeof crypto.randomUUID === "function" ? crypto.randomUUID() : String(Math.random());
  return `${random}-${senderSeq}`;
}

export class UiStateSync {
  readonly scope: UiScope;
  private readonly key: string;
  private readonly url: string;
  private readonly fetchImpl: typeof fetch | undefined;
  private readonly storage: StorageLike | null;
  private readonly createChannel: ((name: string) => BroadcastLike) | null;
  private readonly setTimer: (fn: () => void, ms: number) => unknown;
  private readonly clearTimer: (handle: unknown) => void;
  private mobileOnly: boolean;
  private readonly sender = newSenderId();

  private snapshot: UiStateSnapshot;
  private readonly listeners = new Set<() => void>();
  /** Seções alteradas localmente e ainda não confirmadas pelo servidor. */
  private readonly pending = new Map<string, unknown>();
  private timer: unknown = null;
  private flushing: Promise<void> | null = null;
  private channel: BroadcastLike | null = null;
  private started = false;
  /** `stop()` chamado: nada mais é agendado (inclusive a nova tentativa depois de 5xx). */
  private stopped = false;
  /** PUT em voo (seção, valor e o `rev` em que foi enviado). */
  private inflight: { section: string; value: unknown; baseRev: number } | null = null;

  constructor(scope: UiScope, deps: UiStateDeps = {}) {
    this.scope = scope;
    this.key = scopeKey(scope);
    this.url = uiStateUrl(scope);
    this.fetchImpl = deps.fetchImpl;
    this.storage = deps.storage === undefined ? defaultStorage() : deps.storage;
    this.createChannel = deps.createChannel === undefined ? defaultChannel() : deps.createChannel;
    this.setTimer = deps.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
    this.clearTimer = deps.clearTimer ?? ((handle) => clearTimeout(handle as ReturnType<typeof setTimeout>));
    this.mobileOnly = deps.mobileOnly ?? false;
    const cached = this.readCache();
    this.snapshot = { rev: cached.rev, data: cached.data, ready: false };
  }

  // --- leitura (useSyncExternalStore) ---

  readonly subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  readonly getSnapshot = (): UiStateSnapshot => this.snapshot;

  get(section: string): unknown {
    return getSection(this.snapshot.data, section);
  }

  hasPending(): boolean {
    return this.pending.size > 0;
  }

  // --- ciclo de vida ---

  /** Abre o canal entre abas e lê o servidor. Idempotente. */
  start(): Promise<void> {
    if (this.started) return Promise.resolve();
    this.started = true;
    this.stopped = false;
    if (this.createChannel !== null) {
      try {
        this.channel = this.createChannel(CHANNEL_NAME);
        this.channel.onmessage = (event) => this.onChannelMessage(event.data);
      } catch {
        this.channel = null;
      }
    }
    return this.load();
  }

  /** Fecha o canal e o timer (as seções pendentes continuam, para um `flush` final). */
  stop(): void {
    this.started = false;
    this.stopped = true;
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    if (this.channel !== null) {
      this.channel.onmessage = null;
      this.channel.close();
      this.channel = null;
    }
  }

  /** Relê o servidor e adota o envelope (mantendo as seções pendentes por cima). */
  async load(): Promise<void> {
    const result = await request<unknown>({ method: "GET", path: this.url }, this.fetchImpl);
    if (!result.ok) {
      // Sem servidor: segue com o cache; ainda assim a UI pode ser usada.
      this.publish({ ...this.snapshot, ready: true });
      return;
    }
    this.adopt(parseEnvelope(result.data), false);
  }

  // --- escrita ---

  /**
   * Liga ou desliga o modo celular deste cliente (girar o aparelho troca o modo sem recriar o
   * cliente): ligado, só a seção `mobile` é gravada; as demais são recusadas (`set` → `false`).
   */
  setMobileOnly(on: boolean): void {
    this.mobileOnly = on;
  }

  /** O cliente está no modo celular (só grava a seção `mobile`). */
  isMobileOnly(): boolean {
    return this.mobileOnly;
  }

  /** Troca uma seção localmente e agenda o PUT. Seção não permitida é ignorada (`false`). */
  set(section: string, value: unknown): boolean {
    if (!isSectionAllowed(this.scope, section)) return false;
    if (this.mobileOnly && section !== UI_SECTIONS.mobile) return false;
    // Valor igual ao atual: nada a gravar (evita PUT e vaivém entre abas).
    if (sameJson(this.get(section), value)) return true;
    this.pending.set(section, value);
    this.publish({ ...this.snapshot, data: setSection(this.snapshot.data, section, value) });
    this.writeCache();
    this.schedule();
    return true;
  }

  /** Agenda o próximo PUT: no máximo 1 por segundo; as mudanças do intervalo saem juntas. */
  private schedule(delay: number = UI_STATE_DEBOUNCE_MS): void {
    if (this.stopped || this.timer !== null || this.pending.size === 0) return;
    this.timer = this.setTimer(() => {
      this.timer = null;
      void this.flush();
    }, delay);
  }

  /** Envia agora todas as seções pendentes (uma por PUT, em sequência). */
  flush(): Promise<void> {
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    if (this.flushing !== null) {
      // Termina o envio atual; o que chegou no meio sai na próxima janela de 1 s.
      return this.flushing.then(() => {
        this.schedule();
      });
    }
    const run = this.flushPending().finally(() => {
      this.flushing = null;
    });
    this.flushing = run;
    return run;
  }

  private async flushPending(): Promise<void> {
    let retryLater = false;
    for (const [section, value] of [...this.pending]) {
      const outcome = await this.putSection(section, value);
      if (this.pending.get(section) === value && outcome !== "retry") this.pending.delete(section);
      if (outcome === "retry") retryLater = true;
    }
    if (retryLater) this.schedule(UI_STATE_RETRY_MS);
    else this.schedule();
  }

  /** PUT de uma seção, com reaplicação em 409. */
  private async putSection(section: string, value: unknown): Promise<"done" | "dropped" | "retry"> {
    for (let attempt = 0; attempt <= MAX_CONFLICT_RETRIES; attempt += 1) {
      let conflictBody: unknown = undefined;
      const baseRev = this.snapshot.rev;
      this.inflight = { section, value, baseRev };
      const result = await request<unknown>(
        {
          method: "PUT",
          path: this.url,
          // Sempre `keepalive`: se a aba fechar com este PUT em voo (o `visibilitychange` dispara
          // o envio normal logo antes do `pagehide`), ele sobrevive e a cadeia do `pagehide`
          // continua correta a partir dele. Um PUT por vez, corpo bem abaixo de 64 KiB.
          keepalive: true,
          body: JSON.stringify({ base_rev: baseRev, section, value }),
          onHttpError: (status, body) => {
            if (status === 409) conflictBody = body;
            return null;
          },
        },
        this.fetchImpl,
      );
      this.inflight = null;
      if (result.ok) {
        this.adopt(parseEnvelope(result.data), true);
        return "done";
      }
      const status = result.error.status;
      if (status === 409) {
        // Outro escritor avançou o `rev`: adota o atual e reaplica a própria seção.
        if (conflictBody !== undefined) this.adopt(parseEnvelope(conflictBody), false);
        else await this.load();
        continue;
      }
      if (status === 0 || status >= 500) return "retry";
      // 400/404/413/422: não adianta repetir.
      return "dropped";
    }
    return "retry";
  }

  /**
   * Página saindo: manda as seções pendentes com `keepalive` e esquece-as. Uma por PUT, em ordem
   * de nome, com `base_rev` encadeado (`rev`, `rev+1`, …) para que cada uma seja aceita depois da
   * anterior. Um PUT já em voo conta como o primeiro da cadeia e a sua seção não é reenviada
   * (salvo se mudou depois de sair).
   */
  flushOnPageHide(): void {
    if (this.timer !== null) {
      this.clearTimer(this.timer);
      this.timer = null;
    }
    const inflight = this.inflight;
    const sections = [...this.pending]
      .filter(([section, value]) => !(inflight !== null && inflight.section === section && inflight.value === value))
      .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
    this.pending.clear();
    if (sections.length === 0) return;
    const doFetch = this.fetchImpl ?? (typeof fetch === "function" ? fetch : null);
    if (doFetch === null) return;
    let baseRev = inflight === null ? this.snapshot.rev : inflight.baseRev + 1;
    for (const [section, value] of sections) {
      try {
        void doFetch(this.url, {
          method: "PUT",
          keepalive: true,
          credentials: "same-origin",
          headers: { "Content-Type": "application/json", Accept: "application/json" },
          body: JSON.stringify({ base_rev: baseRev, section, value }),
        }).catch(() => undefined);
      } catch {
        // Sem rede ao sair: o cache local ainda tem a mudança.
      }
      baseRev += 1;
    }
  }

  // --- adoção de envelopes (servidor, 409, outra aba) ---

  private adopt(envelope: UiStateEnvelope, broadcast: boolean): void {
    let data = envelope.data;
    // As seções ainda pendentes (inclusive as alteradas durante o PUT) ficam por cima.
    for (const [section, value] of this.pending) data = setSection(data, section, value);
    // Seções iguais às atuais mantêm a identidade (memos e efeitos da UI não disparam à toa).
    data = shareStructure(this.snapshot.data, data);
    this.publish({ rev: envelope.rev, data, ready: true });
    this.writeCache();
    if (broadcast && this.channel !== null) {
      const message: ChannelMessage = {
        type: "ui-state",
        scope: this.key,
        sender: this.sender,
        rev: envelope.rev,
        data: envelope.data,
      };
      try {
        this.channel.postMessage(message);
      } catch {
        // Canal fechado: a outra aba reconcilia por 409.
      }
    }
  }

  private onChannelMessage(raw: unknown): void {
    if (!isPlainObject(raw) || raw.type !== "ui-state" || raw.scope !== this.key || raw.sender === this.sender) return;
    if (typeof raw.rev !== "number" || !Number.isInteger(raw.rev) || !isPlainObject(raw.data)) return;
    if (raw.rev <= this.snapshot.rev) return;
    this.adopt({ version: UI_STATE_VERSION, rev: raw.rev, data: raw.data }, false);
  }

  // --- cache ---

  private readCache(): { rev: number; data: Record<string, unknown> } {
    if (this.storage === null) return { rev: 0, data: {} };
    try {
      const text = this.storage.getItem(CACHE_PREFIX + this.key);
      if (text === null) return { rev: 0, data: {} };
      const env = parseEnvelope(JSON.parse(text) as unknown);
      return { rev: env.rev, data: env.data };
    } catch {
      this.storage.removeItem(CACHE_PREFIX + this.key);
      return { rev: 0, data: {} };
    }
  }

  private writeCache(): void {
    if (this.storage === null) return;
    try {
      const env: UiStateEnvelope = { version: UI_STATE_VERSION, rev: this.snapshot.rev, data: this.snapshot.data };
      this.storage.setItem(CACHE_PREFIX + this.key, JSON.stringify(env));
    } catch {
      // Cota cheia ou modo privado: o cache é opcional.
    }
  }

  private publish(next: UiStateSnapshot): void {
    this.snapshot = next;
    for (const listener of this.listeners) listener();
  }
}

/** Chave do cache local de um escopo (para limpeza e testes). */
export function uiStateCacheKey(scope: UiScope): string {
  return CACHE_PREFIX + scopeKey(scope);
}
