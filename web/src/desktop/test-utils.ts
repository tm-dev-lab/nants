/*
 * Utilitários de teste do desktop. jsdom não tem layout: aqui ficam os polyfills mínimos
 * (captura de ponteiro), um `ResizeObserver` controlável, um `BroadcastChannel` em memória,
 * um agendador de quadros manual e um `fetch` do ui-state com `rev`/409. Só para testes.
 */
import type { BroadcastLike } from "./uiState";
import type { FrameScheduler } from "./useWindowGesture";
import type { ResizeObserverFactory, ResizeObserverLike } from "./useContainerSize";

/** Monta HTML de teste por DOMParser (a atribuição direta de HTML é proibida em src/ pelo teste de segurança). */
export function setBody(html: string): void {
  const doc = new DOMParser().parseFromString(`<body>${html}</body>`, "text/html");
  document.body.replaceChildren(...Array.from(doc.body.childNodes).map((n) => document.importNode(n, true)));
}

/** `setPointerCapture`/`releasePointerCapture`/`hasPointerCapture` mínimos em `Element`. */
export function installPointerCapturePolyfill(): () => void {
  const proto = Element.prototype as Element & {
    setPointerCapture?: (id: number) => void;
    releasePointerCapture?: (id: number) => void;
    hasPointerCapture?: (id: number) => boolean;
  };
  const original = {
    set: proto.setPointerCapture,
    release: proto.releasePointerCapture,
    has: proto.hasPointerCapture,
  };
  const captured = new WeakMap<Element, Set<number>>();
  proto.setPointerCapture = function setPointerCapture(this: Element, id: number) {
    const set = captured.get(this) ?? new Set<number>();
    set.add(id);
    captured.set(this, set);
  };
  proto.releasePointerCapture = function releasePointerCapture(this: Element, id: number) {
    captured.get(this)?.delete(id);
  };
  proto.hasPointerCapture = function hasPointerCapture(this: Element, id: number) {
    return captured.get(this)?.has(id) ?? false;
  };
  return () => {
    proto.setPointerCapture = original.set;
    proto.releasePointerCapture = original.release;
    proto.hasPointerCapture = original.has;
  };
}

/** Evento de ponteiro com `clientX/Y`, `pointerId`, `button` e `isPrimary` (jsdom os ignora em parte). */
export function pointerEvent(
  type: string,
  init: { clientX?: number; clientY?: number; pointerId?: number; button?: number; isPrimary?: boolean } = {},
): Event {
  const event = new MouseEvent(type, {
    bubbles: true,
    cancelable: true,
    clientX: init.clientX ?? 0,
    clientY: init.clientY ?? 0,
    button: init.button ?? 0,
  });
  Object.defineProperty(event, "pointerId", { value: init.pointerId ?? 1 });
  Object.defineProperty(event, "isPrimary", { value: init.isPrimary ?? true });
  return event;
}

/** Agendador de quadros manual: `runFrames()` executa os callbacks pendentes. */
export function manualScheduler(): FrameScheduler & { runFrames(): number; pending(): number } {
  let next = 1;
  const queue = new Map<number, () => void>();
  return {
    request(callback) {
      const id = next;
      next += 1;
      queue.set(id, callback);
      return id;
    },
    cancel(id) {
      queue.delete(id);
    },
    runFrames() {
      const callbacks = [...queue.values()];
      queue.clear();
      callbacks.forEach((cb) => cb());
      return callbacks.length;
    },
    pending() {
      return queue.size;
    },
  };
}

/** `ResizeObserver` controlável: `resize(width)` notifica todos os observadores ativos. */
export function fakeResizeObservers(): { factory: ResizeObserverFactory; resize(width: number): void; active(): number } {
  const observers = new Set<{ cb: Parameters<ResizeObserverFactory>[0]; observer: ResizeObserverLike }>();
  return {
    factory: (cb) => {
      const entry = {
        cb,
        observer: {
          observe: () => {
            observers.add(entry);
          },
          disconnect: () => {
            observers.delete(entry);
          },
        },
      };
      return entry.observer;
    },
    resize(width) {
      for (const entry of observers) entry.cb([{ contentRect: { width } }]);
    },
    active() {
      return observers.size;
    },
  };
}

/** `BroadcastChannel` em memória: canais do mesmo nome trocam mensagens (sem eco para si). */
export function memoryBroadcastHub(): { create(name: string): BroadcastLike; sent: unknown[] } {
  const channels = new Set<{ name: string; channel: BroadcastLike }>();
  const sent: unknown[] = [];
  return {
    sent,
    create(name) {
      const entry = {
        name,
        channel: {
          onmessage: null as ((event: MessageEvent) => void) | null,
          postMessage(message: unknown) {
            sent.push(message);
            for (const other of channels) {
              if (other !== entry && other.name === name) {
                other.channel.onmessage?.(new MessageEvent("message", { data: message }));
              }
            }
          },
          close() {
            channels.delete(entry);
          },
        },
      };
      channels.add(entry);
      return entry.channel;
    },
  };
}

/** `Storage` em memória (cache do ui-state nos testes). */
export function memoryStorage(initial: Record<string, string> = {}): Storage {
  const map = new Map(Object.entries(initial));
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (key) => map.get(key) ?? null,
    key: (index) => [...map.keys()][index] ?? null,
    removeItem: (key) => {
      map.delete(key);
    },
    setItem: (key, value) => {
      map.set(key, value);
    },
  };
}

interface UiStateServerCall {
  method: string;
  url: string;
  body: unknown;
  keepalive: boolean;
}

/**
 * Servidor de ui-state em memória (mesmo contrato do T01): GET devolve o envelope; PUT com
 * `base_rev` diferente → 409 com o envelope atual; senão grava a seção e `rev + 1`.
 */
export function fakeUiStateServer(initial: { rev?: number; data?: Record<string, unknown>; version?: number } = {}) {
  const state = { version: initial.version ?? 1, rev: initial.rev ?? 0, data: initial.data ?? {} };
  const calls: UiStateServerCall[] = [];
  /** Respostas forçadas (status) para os próximos PUTs. */
  const forced: number[] = [];
  const setPath = (data: Record<string, unknown>, section: string, value: unknown): Record<string, unknown> => {
    const parts = section.split(".");
    const copy: Record<string, unknown> = structuredClone(data);
    let node = copy;
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
  const fetchImpl = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input.toString();
    const method = init?.method ?? "GET";
    const body: unknown = typeof init?.body === "string" ? (JSON.parse(init.body) as unknown) : undefined;
    calls.push({ method, url, body, keepalive: init?.keepalive === true });
    const json = (status: number, payload: unknown): Response =>
      new Response(JSON.stringify(payload), { status, headers: { "Content-Type": "application/json" } });
    if (method === "GET") return json(200, state);
    const status = forced.shift();
    if (status !== undefined) return json(status, { error: "forçado" });
    const put = body as { base_rev: number; section: string; value: unknown };
    if (put.base_rev !== state.rev) return json(409, { ...state, error: "ui_state_conflict" });
    state.data = setPath(state.data, put.section, put.value);
    state.rev += 1;
    return json(200, state);
  }) as typeof fetch;
  return {
    state,
    calls,
    fetchImpl,
    /** Outro escritor (outro dispositivo) muda uma seção direto no servidor. */
    externalWrite(section: string, value: unknown) {
      state.data = setPath(state.data, section, value);
      state.rev += 1;
    },
    forceNext(status: number) {
      forced.push(status);
    },
    puts: () => calls.filter((c) => c.method === "PUT"),
  };
}

/** Uma regra de CSS (sem comentários): seletores separados por vírgula e o corpo. */
interface CssRule {
  selectors: string[];
  body: string;
  /** Condição do `@media`/`@supports` em volta, ou `null` no nível do arquivo. */
  at: string | null;
}

/**
 * Regras de um CSS-fonte (o jsdom não aplica CSS; os testes conferem as regras). Cobre o que os
 * arquivos do desktop usam: regras simples e regras dentro de `@media`/`@supports`.
 */
export function cssRules(css: string): CssRule[] {
  const clean = css.replace(/\/\*[\s\S]*?\*\//g, "");
  const out: CssRule[] = [];
  const walk = (text: string, at: string | null): void => {
    let i = 0;
    while (i < text.length) {
      const open = text.indexOf("{", i);
      if (open === -1) return;
      const head = text.slice(i, open).trim();
      // Fecha no `}` que equilibra a chave aberta.
      let depth = 1;
      let j = open + 1;
      while (j < text.length && depth > 0) {
        if (text[j] === "{") depth += 1;
        else if (text[j] === "}") depth -= 1;
        j += 1;
      }
      const inner = text.slice(open + 1, j - 1);
      if (head.startsWith("@")) {
        if (!head.startsWith("@keyframes")) walk(inner, head);
      } else {
        out.push({ selectors: head.split(",").map((s) => s.trim()).filter((s) => s !== ""), body: inner, at });
      }
      i = j;
    }
  };
  walk(clean, null);
  return out;
}

/** Corpo da primeira regra (fora de `@media`) que tem exatamente este seletor. */
export function cssRuleBody(css: string, selector: string): string {
  const rule = cssRules(css).find((r) => r.at === null && r.selectors.includes(selector));
  if (rule === undefined) throw new Error(`regra ausente: ${selector}`);
  return rule.body;
}
