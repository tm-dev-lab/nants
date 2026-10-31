/*
 * Stream SSE de `GET /api/events` com reconexão exponencial limitada.
 * - Cada evento é validado: envelope objeto, `event_type` conhecido, `project_id`
 *   string ou null, `payload` objeto. Desconhecidos são ignorados.
 * - `lagged` (assinante atrasado) é entregue ao callback próprio; o stream segue.
 * - `close()` encerra a conexão e cancela qualquer reconexão pendente.
 * - `ping` (spec 080): sinal de vida do servidor a cada 15 s; só alimenta o watchdog, nunca o store.
 * - Watchdog: com a aba visível, sem nenhum evento (inclusive `ping`) por 2,5 × o intervalo do ping,
 *   a conexão é tratada como zumbi e reaberta (`reconnectNow`). Com a aba oculta, fica pausado.
 */
import { isMessageCode, parseCodeParams } from "./codes";
import { useEffect, useRef } from "react";
import type { ApiEvent, ApiEventType, LaggedEvent, RecoveryDecision } from "./types";

/** Decisões que o servidor anuncia no `recovery_decided` (spec 124 T03: SPLIT, ADJUST e ASK). */
const RECOVERY_DECISIONS: readonly string[] = [
  "RETRY",
  "REVISE_TASK",
  "REVISE_CONFIG",
  "MIGRATE_ENVIRONMENT",
  "SPLIT",
  "ADJUST",
  "ASK",
  "ESCALATE",
] satisfies readonly RecoveryDecision[];

/** Decisões que pausam a execução à espera do humano (ESCALATE e ASK). */
export function pausesForHuman(decision: RecoveryDecision): boolean {
  return decision === "ESCALATE" || decision === "ASK";
}

const EVENT_STREAM_PATH = "/api/events";
/** Primeiro atraso de reconexão. */
const RECONNECT_BASE_MS = 1_000;
/** Teto do atraso de reconexão. */
const RECONNECT_MAX_MS = 15_000;
/**
 * Tempo que a conexão precisa ficar aberta para contar como estável e zerar o backoff.
 * Um servidor que aceita e derruba logo depois não zera a sequência (evita rajada de GETs).
 */
const STABLE_CONNECTION_MS = 10_000;

/** Nomes de evento conhecidos (além de `lagged`). Qualquer outro é ignorado. */
export const KNOWN_EVENT_TYPES: readonly ApiEventType[] = [
  "project_status",
  "agent_message",
  "agent_activity",
  "chat_turn_started",
  "chat_turn_ended",
  "task_updated",
  "permission_requested",
  "permission_resolved",
  "sdd_draft_updated",
  "project_removed",
  "report_ready",
  "report_failed",
  "finalization_done",
  "recovery_started",
  "recovery_decided",
  "app_started",
  "app_exited",
  "app_mode_changed",
  "app_exec_started",
  "app_exec_output",
  "app_exec_finished",
  "devices_changed",
  "device_code_locked",
  "kb_updated",
  "nanty_changed",
];

const LAGGED = "lagged";
/** Evento de vida do servidor (`data: {}`, sem projeto). Conhecido, mas nunca entregue ao redutor. */
export const PING_EVENT = "ping";
/** Intervalo do `ping` no servidor (`KEEP_ALIVE_INTERVAL`). */
const PING_INTERVAL_MS = 15_000;
/** Sem eventos por este tempo (2,5 × o ping) com a aba visível: conexão zumbi. */
export const WATCHDOG_TIMEOUT_MS = PING_INTERVAL_MS * 2.5;
/** Período de verificação do watchdog. */
const WATCHDOG_CHECK_MS = 5_000;

type StreamStatus = "connecting" | "open" | "reconnecting" | "closed";

/** Subconjunto de EventSource usado pelo cliente (facilita testes). */
export interface EventSourceLike {
  readonly readyState: number;
  onerror: ((ev: Event) => void) | null;
  onopen: ((ev: Event) => void) | null;
  addEventListener(type: string, listener: (ev: MessageEvent<string>) => void): void;
  close(): void;
}

type EventSourceFactory = (url: string) => EventSourceLike;

interface EventStreamOptions {
  projectId?: string;
  onEvent: (event: ApiEvent) => void;
  onLagged?: (info: LaggedEvent) => void;
  onStatus?: (status: StreamStatus) => void;
  createEventSource?: EventSourceFactory;
  baseDelayMs?: number;
  maxDelayMs?: number;
  /** Chamado depois que o watchdog reabriu uma conexão sem eventos (para ressincronizar). */
  onWatchdog?: () => void;
  /** Tempo sem eventos que dispara o watchdog (padrão `WATCHDOG_TIMEOUT_MS`). */
  watchdogMs?: number;
  /** Período de verificação do watchdog (padrão `WATCHDOG_CHECK_MS`). */
  watchdogCheckMs?: number;
  /** Aba visível? O watchdog só age com a aba visível (padrão: `document.visibilityState`). */
  isVisible?: () => boolean;
  /** Relógio (padrão `Date.now`). */
  now?: () => number;
}

function defaultIsVisible(): boolean {
  return typeof document === "undefined" || document.visibilityState !== "hidden";
}

/** Narrowing sem `any`: objeto simples (não nulo, não array). */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isKnownEventType(value: unknown): value is ApiEventType {
  return typeof value === "string" && (KNOWN_EVENT_TYPES as readonly string[]).includes(value);
}

const PROJECT_STATUSES_EMITTED: readonly string[] = ["executing", "integrated", "blocked", "idle"];
const TASK_STATUSES: readonly string[] = [
  "TODO",
  "IN_PROGRESS",
  "IN_REVIEW",
  "CHANGES_REQUESTED",
  "APPROVED",
  "INTEGRATING",
  "DONE",
  "BLOCKED",
];

/** Formas da execução do app (spec 130). */
const APP_MODES: readonly string[] = ["page", "running", "finished"];
const ACTIVITY_KINDS: readonly string[] = ["start", "reasoning", "text", "tool", "command", "heartbeat", "command_end", "model", "model_end", "end"];

function isStr(value: unknown): value is string {
  return typeof value === "string";
}

/** Valida o payload de `project_status` conforme os status que o servidor publica (runs.rs). */
function validProjectStatus(p: Record<string, unknown>): boolean {
  const status = p.status;
  if (!isStr(status) || !PROJECT_STATUSES_EMITTED.includes(status)) return false;
  switch (status) {
    case "executing":
    case "integrated":
      return isStr(p.spec_id);
    case "blocked":
      return isStr(p.task_id) && isStr(p.reason);
    case "idle":
      return (
        isStr(p.reason) &&
        (p.needs_manual_recovery === undefined || typeof p.needs_manual_recovery === "boolean")
      );
    default:
      return false;
  }
}

/** Inteiro seguro não negativo (seq do diálogo; 0 é aceito). */
export function isSafeSeq(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Tentativa da tarefa: inteiro não negativo. */
export function isAttempt(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

/** Campo opcional: ausente ou `null` é válido; presente e não nulo precisa passar em `check`. */
function optionalOrNull(value: unknown, check: (v: unknown) => boolean): boolean {
  return value === undefined || value === null || check(value);
}

/**
 * Campos auxiliares de `agent_message` (`task_id`, `attempt`, `seq`): um valor com tipo errado é
 * removido do payload (fica ausente). A mensagem em si continua válida.
 */
function sanitizeAgentMessage(p: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = { ...p };
  if (!optionalOrNull(p.task_id, isStr)) delete out.task_id;
  if (!optionalOrNull(p.attempt, isAttempt)) delete out.attempt;
  if (!optionalOrNull(p.seq, isSafeSeq)) delete out.seq;
  if (!optionalOrNull(p.spec_id, isStr)) delete out.spec_id;
  // `code`/`params` (spec 102): fora do formato ficam ausentes; params só com textos e números.
  if (isMessageCode(p.code)) {
    const params = parseCodeParams(p.params);
    if (params === null) delete out.params;
    else out.params = params;
  } else {
    delete out.code;
    delete out.params;
  }
  return out;
}

/** `kind` de `blocked` fora de quality/environmental é descartado (o evento segue válido). */
function sanitizeProjectStatus(p: Record<string, unknown>): Record<string, unknown> {
  if (p.kind === undefined || p.kind === "quality" || p.kind === "environmental") return p;
  const out = { ...p };
  delete out.kind;
  return out;
}

/** Valida o payload por tipo de evento. Campos extras são permitidos; campos exigidos, não. */
function validPayload(type: ApiEventType, p: Record<string, unknown>): boolean {
  switch (type) {
    case "project_status":
      return validProjectStatus(p);
    case "agent_message":
      // `turn_id` é opcional, mas quando presente precisa ser texto (senão o evento é inválido).
      // `task_id`, `attempt` e `seq` são auxiliares: campo com tipo errado vira ausente (ver `sanitizeAgentMessage`),
      // para que a mensagem não se perca por causa dele.
      return isStr(p.agent) && isStr(p.text) && (p.turn_id === undefined || isStr(p.turn_id));
    case "agent_activity":
      return (
        isStr(p.agent) &&
        isStr(p.activity_id) &&
        isStr(p.kind) &&
        ACTIVITY_KINDS.includes(p.kind) &&
        optionalOrNull(p.task_id, isStr) &&
        optionalOrNull(p.spec_id, isStr) &&
        (p.text === undefined || isStr(p.text)) &&
        (p.stage === undefined || isStr(p.stage))
      );
    case "chat_turn_started":
      return (
        isStr(p.project_id) &&
        isStr(p.turn_id) &&
        p.turn_id !== "" &&
        (p.user_text === undefined || isStr(p.user_text)) &&
        typeof p.started_at === "number"
      );
    case "chat_turn_ended":
      return isStr(p.project_id) && isStr(p.turn_id) && p.turn_id !== "" && typeof p.ok === "boolean";
    case "task_updated":
      return (
        isStr(p.spec_id) &&
        isStr(p.task_id) &&
        isStr(p.status) &&
        TASK_STATUSES.includes(p.status) &&
        (p.integrated === undefined || typeof p.integrated === "boolean")
      );
    case "permission_requested":
      return (
        isStr(p.request_id) &&
        isStr(p.project_id) &&
        // Chave sempre presente; `null` para comando fora da allowlist (sem tarefa).
        (p.task_id === null || isStr(p.task_id)) &&
        isStr(p.agent) &&
        isStr(p.command) &&
        // Vazios nunca são emitidos pelo servidor (runs.rs/approvals.rs): tratados como inválidos.
        p.request_id !== "" &&
        p.command !== "" &&
        // Campos de escopo são opcionais (servidor antigo); quando presentes, têm o tipo certo.
        (p.always_allowed === undefined || typeof p.always_allowed === "boolean") &&
        (p.always_denied_reason === undefined || p.always_denied_reason === null || isStr(p.always_denied_reason)) &&
        (p.always_denied_code === undefined || p.always_denied_code === null || isStr(p.always_denied_code))
      );
    case "permission_resolved":
      return isStr(p.request_id) && isStr(p.project_id) && isStr(p.applied) && p.request_id !== "";
    case "sdd_draft_updated":
      return isStr(p.project_id);
    case "project_removed":
      return isStr(p.project_id);
    case "report_ready":
      return isStr(p.spec_id);
    case "finalization_done":
      return isStr(p.spec_id);
    case "report_failed":
      return isStr(p.spec_id) && isStr(p.code);
    case "recovery_started":
      return isStr(p.spec_id) && isStr(p.task_id) && isAttempt(p.cycle);
    case "recovery_decided":
      return (
        isStr(p.spec_id) &&
        isStr(p.task_id) &&
        isAttempt(p.cycle) &&
        typeof p.decision === "string" &&
        RECOVERY_DECISIONS.includes(p.decision) &&
        (p.options === undefined || Array.isArray(p.options)) &&
        (p.options_analysis === undefined || p.options_analysis === null || (typeof p.options_analysis === "number" && Number.isFinite(p.options_analysis)))
      );
    case "app_started":
      return isStr(p.spec_id) && optionalOrNull(p.url, isStr);
    case "app_exited":
      return (
        optionalOrNull(p.code, (v) => typeof v === "number" && Number.isSafeInteger(v)) && optionalOrNull(p.reason, isStr)
      );
    case "app_mode_changed":
      return (
        isStr(p.spec_id) &&
        isStr(p.mode) &&
        APP_MODES.includes(p.mode) &&
        optionalOrNull(p.exit_code, (v) => typeof v === "number" && Number.isSafeInteger(v))
      );
    case "app_exec_started":
      return isStr(p.exec_id) && isStr(p.spec_id) && isStr(p.command) && isStr(p.started_at);
    case "app_exec_output":
      return isStr(p.exec_id) && isSafeSeq(p.seq) && isStr(p.text) && typeof p.truncated === "boolean";
    case "app_exec_finished":
      return (
        isStr(p.exec_id) &&
        isStr(p.status) &&
        optionalOrNull(p.exit_code, (v) => typeof v === "number" && Number.isSafeInteger(v))
      );
    case "devices_changed":
    case "device_code_locked":
      return p.reason === undefined || isStr(p.reason);
    case "nanty_changed":
      return isStr(p.part);
    case "kb_updated":
      return (
        isStr(p.id) &&
        (p.removed === true ||
          (isStr(p.status) && optionalOrNull(p.status_reason, isStr) && isRecord(p.doc)))
      );
    default:
      return false;
  }
}

/** Valida um envelope e o payload do seu tipo. Devolve `null` para qualquer coisa fora do contrato. */
export function parseApiEvent(raw: unknown): ApiEvent | null {
  if (!isRecord(raw)) return null;
  const { event_type, project_id, payload } = raw;
  if (!isKnownEventType(event_type)) return null;
  if (project_id !== null && typeof project_id !== "string") return null;
  if (!isRecord(payload)) return null;
  if (!validPayload(event_type, payload)) return null;
  const clean =
    event_type === "agent_message"
      ? sanitizeAgentMessage(payload)
      : event_type === "project_status"
        ? sanitizeProjectStatus(payload)
        : event_type === "app_started"
          ? { ...payload, url: isStr(payload.url) ? payload.url : null }
          : event_type === "app_exited"
            ? {
                code: typeof payload.code === "number" ? payload.code : null,
                reason: isStr(payload.reason) ? payload.reason : "exited",
              }
            : event_type === "app_mode_changed"
              ? {
                  spec_id: payload.spec_id,
                  mode: payload.mode,
                  previous: isStr(payload.previous) && APP_MODES.includes(payload.previous) ? payload.previous : null,
                  exit_code: typeof payload.exit_code === "number" ? payload.exit_code : null,
                }
            : event_type === "devices_changed" || event_type === "device_code_locked"
              ? { reason: isStr(payload.reason) ? payload.reason : "" }
              : payload;
  return { event_type, project_id, payload: clean } as ApiEvent;
}

/** Decodifica o `data` de um evento SSE e valida o envelope. JSON inválido → `null`. */
export function parseEventData(data: string): ApiEvent | null {
  let decoded: unknown;
  try {
    decoded = JSON.parse(data) as unknown;
  } catch {
    return null;
  }
  return parseApiEvent(decoded);
}

/** Decodifica o `data` do evento `lagged` (`{"skipped": n}`). */
export function parseLagged(data: string): LaggedEvent | null {
  let decoded: unknown;
  try {
    decoded = JSON.parse(data) as unknown;
  } catch {
    return null;
  }
  if (!isRecord(decoded)) return null;
  const skipped = decoded.skipped;
  return typeof skipped === "number" && Number.isFinite(skipped) && skipped >= 0
    ? { skipped }
    : null;
}

/** URL do stream; `project_id` só entra quando informado (o servidor valida o formato). */
export function eventStreamUrl(projectId?: string): string {
  if (projectId === undefined || projectId === "") return EVENT_STREAM_PATH;
  return `${EVENT_STREAM_PATH}?project_id=${encodeURIComponent(projectId)}`;
}

/** Atraso antes da tentativa `attempt` (0 = primeira reconexão): 1 s, 2 s, 4 s … até 15 s. */
export function reconnectDelay(attempt: number, baseMs = RECONNECT_BASE_MS, maxMs = RECONNECT_MAX_MS): number {
  const exp = baseMs * 2 ** Math.min(attempt, 30);
  return Math.min(exp, maxMs);
}

const defaultFactory: EventSourceFactory = (url) =>
  new EventSource(url, { withCredentials: false }) as unknown as EventSourceLike;

/** Cliente de stream com reconexão. Uma instância por assinatura. */
export class EventStreamClient {
  private source: EventSourceLike | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stableTimer: ReturnType<typeof setTimeout> | null = null;
  private watchdog: ReturnType<typeof setInterval> | null = null;
  /** Horário do último sinal da conexão atual (abertura, evento, `ping` ou `lagged`). */
  private lastEventAt = 0;
  private attempt = 0;
  private closed = true;
  private readonly url: string;
  private readonly options: EventStreamOptions;

  constructor(options: EventStreamOptions) {
    this.options = options;
    this.url = eventStreamUrl(options.projectId);
  }

  /** Abre a conexão. Chamar de novo após `close()` reinicia o stream. */
  start(): void {
    if (!this.closed) return;
    this.closed = false;
    this.attempt = 0;
    this.connect("connecting");
    this.startWatchdog();
  }

  /**
   * Fecha a conexão atual e abre outra já (sem backoff). Usado na retomada da aba e pelo watchdog:
   * o EventSource pode estar "aberto" sem entregar nada. Sem efeito depois de `close()`.
   */
  reconnectNow(): void {
    if (this.closed) return;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.detach();
    this.attempt = 0;
    this.connect("connecting");
  }

  /** Horário (relógio de `now`) do último sinal recebido na conexão atual. */
  lastEventTime(): number {
    return this.lastEventAt;
  }

  /** Encerra o stream e cancela reconexões pendentes. Idempotente. */
  close(): void {
    this.closed = true;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    if (this.watchdog !== null) {
      clearInterval(this.watchdog);
      this.watchdog = null;
    }
    this.detach();
    this.options.onStatus?.("closed");
  }

  private clock(): number {
    return (this.options.now ?? Date.now)();
  }

  private touch(): void {
    this.lastEventAt = this.clock();
  }

  private startWatchdog(): void {
    if (this.watchdog !== null) return;
    const period = this.options.watchdogCheckMs ?? WATCHDOG_CHECK_MS;
    this.watchdog = setInterval(() => this.checkWatchdog(), period);
  }

  /** Só com conexão (não durante o backoff) e aba visível; oculta, o prazo não conta. */
  private checkWatchdog(): void {
    if (this.closed || this.source === null) return;
    const visible = (this.options.isVisible ?? defaultIsVisible)();
    if (!visible) {
      // Pausado: ao voltar, o prazo recomeça (a retomada da aba cuida da ressincronização).
      this.touch();
      return;
    }
    const limit = this.options.watchdogMs ?? WATCHDOG_TIMEOUT_MS;
    if (this.clock() - this.lastEventAt < limit) return;
    this.reconnectNow();
    this.options.onWatchdog?.();
  }

  private connect(status: StreamStatus): void {
    this.options.onStatus?.(status);
    const factory = this.options.createEventSource ?? defaultFactory;
    const source = factory(this.url);
    this.source = source;
    this.touch();

    source.onopen = () => {
      if (this.source !== source) return;
      this.touch();
      // Só zera o backoff após um intervalo estável (ver STABLE_CONNECTION_MS).
      this.clearStableTimer();
      this.stableTimer = setTimeout(() => {
        this.stableTimer = null;
        if (this.source === source) this.attempt = 0;
      }, STABLE_CONNECTION_MS);
      this.options.onStatus?.("open");
    };
    source.onerror = () => {
      if (this.source !== source || this.closed) return;
      this.detach();
      this.scheduleReconnect();
    };
    for (const type of KNOWN_EVENT_TYPES) {
      source.addEventListener(type, (ev) => this.handleMessage(source, ev));
    }
    source.addEventListener(LAGGED, (ev) => this.handleLagged(source, ev));
    source.addEventListener(PING_EVENT, () => {
      if (this.source === source && !this.closed) this.touch();
    });
  }

  private handleMessage(source: EventSourceLike, ev: MessageEvent<string>): void {
    if (this.source !== source || this.closed) return;
    this.touch();
    const parsed = parseEventData(String(ev.data));
    if (parsed !== null) this.options.onEvent(parsed);
  }

  private handleLagged(source: EventSourceLike, ev: MessageEvent<string>): void {
    if (this.source !== source || this.closed) return;
    this.touch();
    const info = parseLagged(String(ev.data));
    if (info !== null) this.options.onLagged?.(info);
  }

  private scheduleReconnect(): void {
    if (this.closed) return;
    const delay = reconnectDelay(
      this.attempt,
      this.options.baseDelayMs,
      this.options.maxDelayMs,
    );
    this.attempt += 1;
    this.options.onStatus?.("reconnecting");
    this.timer = setTimeout(() => {
      this.timer = null;
      if (!this.closed) this.connect("connecting");
    }, delay);
  }

  private clearStableTimer(): void {
    if (this.stableTimer !== null) {
      clearTimeout(this.stableTimer);
      this.stableTimer = null;
    }
  }

  private detach(): void {
    this.clearStableTimer();
    if (this.source !== null) {
      this.source.onopen = null;
      this.source.onerror = null;
      this.source.close();
      this.source = null;
    }
  }
}

/** Controle do stream assinado por `useEventStream`. */
interface EventStreamHandle {
  /** Fecha e reabre a conexão já (ver `EventStreamClient.reconnectNow`). */
  reconnectNow(): void;
}

/**
 * Assina o stream do projeto (ou global, sem `projectId`) enquanto o componente existir.
 * Os callbacks mais recentes são usados sem reabrir a conexão. Devolve um controle estável.
 */
export function useEventStream(
  projectId: string | undefined,
  handlers: Omit<
    EventStreamOptions,
    "projectId" | "createEventSource" | "baseDelayMs" | "maxDelayMs" | "watchdogMs" | "watchdogCheckMs" | "isVisible" | "now"
  >,
): EventStreamHandle {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;
  const clientRef = useRef<EventStreamClient | null>(null);
  const handleRef = useRef<EventStreamHandle>({ reconnectNow: () => clientRef.current?.reconnectNow() });

  useEffect(() => {
    const client = new EventStreamClient({
      projectId,
      onEvent: (ev) => handlersRef.current.onEvent(ev),
      onLagged: (info) => handlersRef.current.onLagged?.(info),
      onStatus: (status) => handlersRef.current.onStatus?.(status),
      onWatchdog: () => handlersRef.current.onWatchdog?.(),
    });
    clientRef.current = client;
    client.start();
    return () => {
      client.close();
      if (clientRef.current === client) clientRef.current = null;
    };
  }, [projectId]);

  return handleRef.current;
}
