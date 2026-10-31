/*
 * Leitura do Nanty pela API (spec 138 T01, T04 e T10), validada campo a campo:
 * - `GET /api/nanty/status`: a instância, o estado do avatar, a saúde do Telegram, o orçamento do
 *   dia e os projetos observados;
 * - `GET /api/nanty/feed`: os cartões esperando, o feito, o previsto e o resumo de hoje;
 * - `GET /api/nanty/conversation`: a conversa única (janela e Telegram), com a origem e o áudio;
 * - `GET /api/nanty/usage`: o gasto do Nanty, à parte dos projetos (a janela Custos);
 * - `GET /api/nanty/config`: a hora do resumo da manhã (o "previsto" do feed) e, na T11, a
 *   configuração inteira das abas Regras e Ajustes (`PUT /api/nanty/config` só com o que mudou e
 *   `PUT /api/nanty/actions/:id` com o nível);
 * - `GET /api/nanty/telegram` e os POST `token`, `pair`, `unpair` e `test` (T11): o canal, só com a
 *   máscara do token; o código e o link de pareamento só na forma `t.me/<bot>?start=<código>`;
 * - `POST /api/nanty/cards/:id/decide`, `POST /api/nanty/undo/:id` e `POST /api/nanty/conversation`.
 *
 * Tudo o que a tela mostra chega como `code` + `params` (C-08) e a web traduz (`nantyText`). O que
 * não confere é descartado.
 */
import { request, type ApiFailure, type ApiResult } from "../../api/client";
import { parseCodeParams, isMessageCode, type CodeParams, type Coded } from "../../api/codes";
import { ptBR } from "../../i18n/ptBR";
import type { Effort } from "../../api/types";
import type { NantyMood } from "./NantyAvatar";

type NantyInstance = "not_started" | "active" | "other_instance" | "unavailable" | "stopped";

/** Saúde do canal do Telegram (T06). */
export type NantyHealth = "off" | "not_configured" | "connecting" | "connected" | "invalid_token" | "conflict" | "network_error" | "rate_limited";

/** Orçamento do dia (T05): em US$ com preço, senão em tokens. */
interface NantyBudget {
  mode: "usd" | "tokens";
  spent_usd: number;
  budget_usd: number;
  tokens: number;
  budget_tokens: number;
  unpriced_turns: number;
  /** O modelo escolhido não tem preço (conta tokens). */
  unpriced: boolean;
  exhausted: boolean;
}

export interface NantyStatus {
  instance: NantyInstance;
  avatar: NantyMood;
  model_configured: boolean;
  telegram_configured: boolean;
  telegram_paired: boolean;
  /** Ausente em servidor antigo. */
  telegram?: NantyHealth;
  budget?: NantyBudget;
  projects_observed?: number;
}

/** Gasto de um dia (T05 e T09). */
export interface NantySpend {
  day: string;
  usd: number;
  input_tokens: number;
  output_tokens: number;
  unpriced_turns: number;
  voice_calls: number;
}

/** Cartão aberto: o aviso, a ação sugerida e o token da decisão. */
export interface NantyCard {
  id: string;
  project_id: string | null;
  /** Grupo do filtro: `<projeto ou *>:<tipo>`. */
  group: string;
  /** O porquê (`nanty.why.*` ou `nanty.card.*`, C-06). */
  code: string;
  params?: CodeParams;
  /** Ação do catálogo (T03). */
  action?: string;
  /** Token da decisão; sem ele, o cartão não tem botões. */
  token?: string;
  created_at_ms?: number;
  expires_at_ms: number;
}

/** Desfazer de um "feito" (T03). */
export interface NantyUndo {
  id: string;
  token: string;
  expires_at_ms: number;
}

type NantyDecidedBy = "auto" | "telegram" | "web";

/** Algo que o Nanty fez ou a pessoa decidiu. */
export interface NantyDone {
  at_ms: number;
  project_id: string | null;
  code: string;
  params: CodeParams;
  /** Parâmetros do erro (`params.error` é o código `serverError.*`). */
  error_params: CodeParams;
  action: string | null;
  decided_by: NantyDecidedBy | null;
  /** `ok`, `failed`, `expired`, `resolved`, `undone`, `config_changed`... */
  result: string;
  undo: NantyUndo | null;
}

/** O que o Nanty fará sozinho se nada mudar. */
export interface NantyPlanned {
  project_id: string | null;
  action: string;
  code: string;
  params: CodeParams;
  at_ms: number;
}

export interface NantyFeed {
  waiting: NantyCard[];
  done?: NantyDone[];
  planned?: NantyPlanned[];
  today: { spend_usd: number; budget_usd: number };
}

/** O áudio de uma mensagem (T09): Opus em OGG, servido por `GET /api/nanty/audio/:id`. */
export interface NantyAudio {
  id: string;
  duration_secs: number;
  bytes: number;
}

/** Mensagem da conversa única (janela e Telegram). */
export interface NantyMessage {
  id: string;
  at_ms: number;
  from: "person" | "nanty";
  channel: "web" | "telegram";
  project_id: string | null;
  /** Texto da pessoa (redigido) ou do modelo, como veio. */
  text: string | null;
  /** Texto fixo do Nanty, pelo dicionário. */
  code: string | null;
  params: CodeParams;
  /** Linhas extras (`params.lines`), cada uma com o seu código. */
  lines: Coded[];
  card_ids: string[];
  /** A pessoa falou por nota de voz: o `text` é a transcrição. */
  voice: boolean;
  audio: NantyAudio | null;
}

export interface NantyUsage {
  budget: NantyBudget | null;
  today: NantySpend;
  /** Dias anteriores, do mais novo para o mais velho. */
  history: NantySpend[];
}

/** Resultado de uma decisão ou de um desfazer: o que o cartão diz (`nanty.outcome.*`). */
export interface NantyOutcome {
  outcome: string;
  code: string;
  params: CodeParams;
  error_params: CodeParams;
}

const INSTANCES: readonly string[] = ["not_started", "active", "other_instance", "unavailable", "stopped"];
const MOODS: readonly string[] = ["attentive", "speaking", "thinking", "sleeping", "waiting"];
const HEALTH: readonly string[] = ["off", "not_configured", "connecting", "connected", "invalid_token", "conflict", "network_error", "rate_limited"];
const DECIDERS: readonly string[] = ["auto", "telegram", "web"];
/** Id de cartão, desfazer e áudio: 16 hex (o `new_id` do servidor). */
const HEX_ID = /^[0-9a-f]{16}$/i;
/** Teto de texto de uma mensagem (o do Telegram, com folga). */
const MAX_TEXT = 8192;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const str = (value: unknown, max = 256): string | null => (typeof value === "string" && value.length > 0 && value.length <= max ? value : null);
const num = (value: unknown): number | null => (typeof value === "number" && Number.isFinite(value) ? value : null);
const count = (value: unknown): number => {
  const n = num(value);
  return n !== null && n >= 0 ? n : 0;
};
const params = (value: unknown): CodeParams => parseCodeParams(value) ?? {};
const errorParams = (value: unknown): CodeParams => (isRecord(value) ? params(value.error_params) : {});

function parseBudget(value: unknown): NantyBudget | null {
  if (!isRecord(value) || (value.mode !== "usd" && value.mode !== "tokens")) return null;
  return {
    mode: value.mode,
    spent_usd: count(value.spent_usd),
    budget_usd: count(value.budget_usd),
    tokens: count(value.tokens),
    budget_tokens: count(value.budget_tokens),
    unpriced_turns: count(value.unpriced_turns),
    unpriced: value.unpriced === true,
    exhausted: value.exhausted === true,
  };
}

export function parseNantyStatus(value: unknown): NantyStatus | null {
  if (!isRecord(value)) return null;
  if (typeof value.instance !== "string" || !INSTANCES.includes(value.instance)) return null;
  if (typeof value.avatar !== "string" || !MOODS.includes(value.avatar)) return null;
  const budget = parseBudget(value.budget);
  const projects = num(value.projects_observed);
  return {
    instance: value.instance as NantyInstance,
    avatar: value.avatar as NantyMood,
    model_configured: value.model_configured === true,
    telegram_configured: value.telegram_configured === true,
    telegram_paired: value.telegram_paired === true,
    ...(typeof value.telegram === "string" && HEALTH.includes(value.telegram) ? { telegram: value.telegram as NantyHealth } : {}),
    ...(budget !== null ? { budget } : {}),
    ...(projects !== null && projects >= 0 ? { projects_observed: projects } : {}),
  };
}

function parseCard(value: unknown): NantyCard | null {
  if (!isRecord(value)) return null;
  const id = str(value.id, 64);
  const group = str(value.group, 256);
  const code = str(value.code, 128);
  const expires = num(value.expires_at_ms);
  if (id === null || group === null || code === null || expires === null) return null;
  const action = str(value.action, 64);
  const token = str(value.token, 128);
  const created = num(value.created_at_ms);
  const cardParams = parseCodeParams(value.params);
  return {
    id,
    project_id: str(value.project_id, 128),
    group,
    code,
    ...(cardParams !== null && Object.keys(cardParams).length > 0 ? { params: cardParams } : {}),
    ...(action !== null ? { action } : {}),
    ...(token !== null ? { token } : {}),
    ...(created !== null ? { created_at_ms: created } : {}),
    expires_at_ms: expires,
  };
}

function parseUndo(value: unknown): NantyUndo | null {
  if (!isRecord(value)) return null;
  const id = str(value.id, 64);
  const token = str(value.token, 128);
  const expires = num(value.expires_at_ms);
  if (id === null || !HEX_ID.test(id) || token === null || expires === null) return null;
  return { id, token, expires_at_ms: expires };
}

function parseDone(value: unknown): NantyDone | null {
  if (!isRecord(value)) return null;
  const at = num(value.at_ms);
  const code = str(value.code, 128);
  const result = str(value.result, 64);
  if (at === null || code === null || result === null) return null;
  const by = typeof value.decided_by === "string" && DECIDERS.includes(value.decided_by) ? (value.decided_by as NantyDecidedBy) : null;
  return {
    at_ms: at,
    project_id: str(value.project_id, 128),
    code,
    params: params(value.params),
    error_params: isRecord(value.params) ? params(value.params.error_params) : {},
    action: str(value.action, 64),
    decided_by: by,
    result,
    undo: parseUndo(value.undo),
  };
}

function parsePlanned(value: unknown): NantyPlanned | null {
  if (!isRecord(value)) return null;
  const action = str(value.action, 64);
  const code = str(value.code, 128);
  const at = num(value.at_ms);
  if (action === null || code === null || at === null) return null;
  return { project_id: str(value.project_id, 128), action, code, params: params(value.params), at_ms: at };
}

function list<T>(value: unknown, parse: (v: unknown) => T | null): T[] {
  return Array.isArray(value) ? value.map(parse).filter((v): v is T => v !== null) : [];
}

export function parseNantyFeed(value: unknown): NantyFeed | null {
  if (!isRecord(value) || !Array.isArray(value.waiting)) return null;
  const today = isRecord(value.today) ? value.today : {};
  const spend = isRecord(today.spend) ? num(today.spend.usd) : null;
  return {
    waiting: list(value.waiting, parseCard),
    done: list(value.done, parseDone),
    planned: list(value.planned, parsePlanned),
    today: { spend_usd: spend ?? 0, budget_usd: num(today.budget_usd) ?? 0 },
  };
}

function parseLines(value: unknown): Coded[] {
  if (!isRecord(value) || !Array.isArray(value.lines)) return [];
  return value.lines.flatMap((line: unknown) => (isRecord(line) && isMessageCode(line.code) ? [{ code: line.code, params: params(line.params) }] : []));
}

function parseAudio(value: unknown): NantyAudio | null {
  if (!isRecord(value)) return null;
  const id = str(value.id, 16);
  if (id === null || !HEX_ID.test(id)) return null;
  return { id, duration_secs: count(value.duration_secs), bytes: count(value.bytes) };
}

function parseMessage(value: unknown): NantyMessage | null {
  if (!isRecord(value)) return null;
  const id = str(value.id, 64);
  const at = num(value.at_ms);
  if (id === null || at === null) return null;
  if (value.from !== "person" && value.from !== "nanty") return null;
  if (value.channel !== "web" && value.channel !== "telegram") return null;
  const code = isMessageCode(value.code) ? value.code : null;
  return {
    id,
    at_ms: at,
    from: value.from,
    channel: value.channel,
    project_id: str(value.project_id, 128),
    text: typeof value.text === "string" && value.text.length <= MAX_TEXT ? value.text : null,
    code,
    params: params(value.params),
    lines: parseLines(value.params),
    card_ids: Array.isArray(value.card_ids) ? value.card_ids.filter((c): c is string => str(c, 64) !== null) : [],
    voice: value.voice === true,
    audio: parseAudio(value.audio),
  };
}

export function parseNantyConversation(value: unknown): NantyMessage[] | null {
  if (!isRecord(value) || !Array.isArray(value.messages)) return null;
  return list(value.messages, parseMessage);
}

function parseSpend(value: unknown): NantySpend {
  const v = isRecord(value) ? value : {};
  return {
    day: str(v.day, 32) ?? "",
    usd: count(v.usd),
    input_tokens: count(v.input_tokens),
    output_tokens: count(v.output_tokens),
    unpriced_turns: count(v.unpriced_turns),
    voice_calls: count(v.voice_calls),
  };
}

export function parseNantyUsage(value: unknown): NantyUsage | null {
  if (!isRecord(value) || !isRecord(value.today)) return null;
  return { budget: parseBudget(value.budget), today: parseSpend(value.today), history: Array.isArray(value.history) ? value.history.map(parseSpend) : [] };
}

function parseNantyOutcome(value: unknown): NantyOutcome | null {
  if (!isRecord(value)) return null;
  const outcome = str(value.outcome, 64);
  const code = isMessageCode(value.code) ? value.code : null;
  if (outcome === null || code === null) return null;
  return { outcome, code, params: params(value.params), error_params: isRecord(value.params) ? errorParams(value.params) : {} };
}

/** A hora do resumo da manhã (`HH:MM`), da configuração. */
function parseDigestTime(value: unknown): string | null {
  if (!isRecord(value)) return null;
  const time = str(value.digest_time, 5);
  return time !== null && /^\d{2}:\d{2}$/.test(time) ? time : null;
}

const INVALID: ApiFailure = {
  kind: "invalid_response",
  status: 200,
  message: ptBR["api.invalidResponse"],
  messageKey: "api.invalidResponse",
};

function parsed<T>(parse: (value: unknown) => T | null) {
  return (r: ApiResult<unknown>): ApiResult<T> => {
    if (!r.ok) return r;
    const data = parse(r.data);
    return data === null ? { ok: false, error: { ...INVALID } } : { ok: true, data };
  };
}

export function getNantyStatus(): Promise<ApiResult<NantyStatus>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/status" }).then(parsed(parseNantyStatus));
}

export function getNantyFeed(): Promise<ApiResult<NantyFeed>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/feed" }).then(parsed(parseNantyFeed));
}

export function getNantyConversation(): Promise<ApiResult<NantyMessage[]>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/conversation" }).then(parsed(parseNantyConversation));
}

export function getNantyUsage(): Promise<ApiResult<NantyUsage>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/usage" }).then(parsed(parseNantyUsage));
}

export function getNantyDigestTime(): Promise<ApiResult<string>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/config" }).then(parsed(parseDigestTime));
}

export type NantyAnswer = "yes" | "no" | "later";

export function decideNantyCard(id: string, token: string, answer: NantyAnswer): Promise<ApiResult<NantyOutcome>> {
  return request<unknown>({ method: "POST", path: `/api/nanty/cards/${encodeURIComponent(id)}/decide`, body: JSON.stringify({ token, answer }) }).then(
    parsed(parseNantyOutcome),
  );
}

export function undoNanty(id: string, token: string): Promise<ApiResult<NantyOutcome>> {
  return request<unknown>({ method: "POST", path: `/api/nanty/undo/${encodeURIComponent(id)}`, body: JSON.stringify({ token }) }).then(parsed(parseNantyOutcome));
}

export function sendNantyMessage(text: string): Promise<ApiResult<unknown>> {
  return request<unknown>({ method: "POST", path: "/api/nanty/conversation", body: JSON.stringify({ text }) });
}

/** Endereço do áudio de uma mensagem (o id já foi validado na leitura). */
export function nantyAudioUrl(audio: NantyAudio): string {
  return `/api/nanty/audio/${audio.id}`;
}

// --- Regras e Ajustes (T11) ---

export type NantyLevel = "off" | "ask" | "auto";
export type NantyRole = "architect" | "developer" | "reviewer";

/** Uma ação do catálogo com nível: as que o Nanty executa e a faixa "só você". */
export interface NantyCatalogEntry {
  id: string;
  kind: "executes" | "person_only";
  level: NantyLevel;
  default_level: NantyLevel;
}

export interface NantyRoute {
  connection_id: string;
  model: string;
}

export interface NantyVoiceRoute extends NantyRoute {
  voice: string;
}

interface NantyVoiceSettings {
  enabled: boolean;
  order: NantyVoiceRoute[];
  transcribe: NantyRoute;
  max_input_secs: number;
}

/** A configuração do Nanty (`GET /api/nanty/config`), sem o token (só o canal o mostra, mascarado). */
export interface NantySettings {
  /** Hora local do Nants agora (`HH:MM`, C-15), lida do servidor; `null` em servidor antigo. */
  now: string | null;
  connection_id: string;
  model: string;
  effort: Effort;
  daily_budget_usd: number;
  daily_budget_tokens: number;
  quiet_start: string;
  quiet_end: string;
  digest_time: string;
  digest_empty: "short" | "skip";
  lang: "pt-BR" | "en";
  excluded_projects: string[];
  catalog: NantyCatalogEntry[];
  reserve: Partial<Record<NantyRole, NantyRoute>>;
  voice: NantyVoiceSettings;
  telegram: { paired: boolean; hold_while_visible: boolean; visible_hold_secs: number };
}

/** O pedido de `PUT /api/nanty/config`: só o que mudou. */
export interface NantySettingsPatch {
  connection_id?: string;
  model?: string;
  effort?: Effort;
  daily_budget_usd?: number;
  daily_budget_tokens?: number;
  quiet_start?: string;
  quiet_end?: string;
  digest_time?: string;
  digest_empty?: "short" | "skip";
  excluded_projects?: string[];
  reserve?: Partial<Record<NantyRole, NantyRoute>>;
  voice?: Partial<NantyVoiceSettings>;
  telegram?: { hold_while_visible?: boolean; visible_hold_secs?: number };
}

/** A janela de pareamento do Telegram: o código e o link só enquanto aberta. */
interface NantyPairing {
  state: "closed" | "open" | "locked";
  code: string | null;
  link: string | null;
  seconds_to_next: number | null;
  expires_in: number | null;
}

/** O canal do Telegram (`GET /api/nanty/telegram`): nunca o token, só a máscara. */
export interface NantyTelegram {
  health: NantyHealth;
  bot_username: string | null;
  bot_token_masked: string;
  configured: boolean;
  paired: boolean;
  pairing: NantyPairing;
}

const LEVELS: readonly string[] = ["off", "ask", "auto"];
const EFFORTS: readonly string[] = ["low", "medium", "high"];
export const NANTY_ROLES: readonly NantyRole[] = ["architect", "developer", "reviewer"];
const HHMM = /^\d{2}:\d{2}$/;
/** Código de pareamento: 16 de `A-Za-z0-9_-` (o alfabeto do `start` do Telegram). */
const PAIR_CODE = /^[A-Za-z0-9_-]{16}$/;
const BOT_NAME = /^[A-Za-z0-9_]{1,64}$/;

const plain = (value: unknown, max = 128): string => (typeof value === "string" && value.length <= max ? value : "");
const clockOf = (value: unknown): string | null => (typeof value === "string" && HHMM.test(value) ? value : null);

function parseRoute(value: unknown): NantyRoute | null {
  if (!isRecord(value)) return null;
  return { connection_id: plain(value.connection_id, 64), model: plain(value.model) };
}

function parseVoiceRoute(value: unknown): NantyVoiceRoute | null {
  const route = parseRoute(value);
  if (route === null || route.connection_id === "" || !isRecord(value)) return null;
  return { ...route, voice: plain(value.voice) };
}

function parseCatalogEntry(value: unknown): NantyCatalogEntry | null {
  if (!isRecord(value)) return null;
  const id = str(value.id, 64);
  if (id === null || (value.kind !== "executes" && value.kind !== "person_only")) return null;
  if (typeof value.level !== "string" || !LEVELS.includes(value.level)) return null;
  if (typeof value.default_level !== "string" || !LEVELS.includes(value.default_level)) return null;
  return { id, kind: value.kind, level: value.level as NantyLevel, default_level: value.default_level as NantyLevel };
}

export function parseNantySettings(value: unknown): NantySettings | null {
  if (!isRecord(value)) return null;
  const quietStart = clockOf(value.quiet_start);
  const quietEnd = clockOf(value.quiet_end);
  const digest = clockOf(value.digest_time);
  if (quietStart === null || quietEnd === null || digest === null) return null;
  if (typeof value.effort !== "string" || !EFFORTS.includes(value.effort)) return null;
  const voice = isRecord(value.voice) ? value.voice : {};
  const telegram = isRecord(value.telegram) ? value.telegram : {};
  const reserve: Partial<Record<NantyRole, NantyRoute>> = {};
  if (isRecord(value.reserve)) {
    for (const role of NANTY_ROLES) {
      const route = parseRoute(value.reserve[role]);
      if (route !== null && route.connection_id !== "") reserve[role] = route;
    }
  }
  return {
    now: clockOf(value.now),
    connection_id: plain(value.connection_id, 64),
    model: plain(value.model),
    effort: value.effort as Effort,
    daily_budget_usd: count(value.daily_budget_usd),
    daily_budget_tokens: count(value.daily_budget_tokens),
    quiet_start: quietStart,
    quiet_end: quietEnd,
    digest_time: digest,
    digest_empty: value.digest_empty === "skip" ? "skip" : "short",
    lang: value.lang === "en" ? "en" : "pt-BR",
    excluded_projects: Array.isArray(value.excluded_projects) ? value.excluded_projects.filter((p): p is string => str(p, 64) !== null) : [],
    catalog: list(value.catalog, parseCatalogEntry),
    reserve,
    voice: {
      enabled: voice.enabled === true,
      order: list(voice.order, parseVoiceRoute),
      transcribe: parseRoute(voice.transcribe) ?? { connection_id: "", model: "" },
      max_input_secs: count(voice.max_input_secs),
    },
    telegram: {
      paired: telegram.paired === true,
      hold_while_visible: telegram.hold_while_visible !== false,
      visible_hold_secs: count(telegram.visible_hold_secs),
    },
  };
}

/** O link de início só vale com a forma `https://t.me/<bot>?start=<código>` e o próprio código. */
function validStartLink(link: unknown, code: string): link is string {
  if (typeof link !== "string") return false;
  const m = /^https:\/\/t\.me\/([^?/]+)\?start=(.+)$/.exec(link);
  return m !== null && BOT_NAME.test(m[1] ?? "") && m[2] === code;
}

function parsePairing(value: unknown): NantyPairing {
  const v = isRecord(value) ? value : {};
  const code = v.state === "open" && typeof v.code === "string" && PAIR_CODE.test(v.code) ? v.code : null;
  const next = num(v.seconds_to_next);
  const expires = num(v.expires_in);
  return {
    state: v.state === "locked" ? "locked" : code !== null ? "open" : "closed",
    code,
    link: code !== null && validStartLink(v.link, code) ? v.link : null,
    seconds_to_next: code !== null && next !== null && next >= 0 ? next : null,
    expires_in: code !== null && expires !== null && expires >= 0 ? expires : null,
  };
}

export function parseNantyTelegram(value: unknown): NantyTelegram | null {
  if (!isRecord(value)) return null;
  if (typeof value.health !== "string" || !HEALTH.includes(value.health)) return null;
  return {
    health: value.health as NantyHealth,
    bot_username: typeof value.bot_username === "string" && BOT_NAME.test(value.bot_username) ? value.bot_username : null,
    bot_token_masked: plain(value.bot_token_masked, 80),
    configured: value.configured === true,
    paired: value.paired === true,
    pairing: parsePairing(value.pairing),
  };
}

export function getNantySettings(): Promise<ApiResult<NantySettings>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/config" }).then(parsed(parseNantySettings));
}

export function putNantySettings(patch: NantySettingsPatch): Promise<ApiResult<NantySettings>> {
  return request<unknown>({ method: "PUT", path: "/api/nanty/config", body: JSON.stringify(patch) }).then(parsed(parseNantySettings));
}

export function putNantyLevel(id: string, level: NantyLevel): Promise<ApiResult<NantySettings>> {
  return request<unknown>({ method: "PUT", path: `/api/nanty/actions/${encodeURIComponent(id)}`, body: JSON.stringify({ level }) }).then(
    parsed(parseNantySettings),
  );
}

export function getNantyTelegram(): Promise<ApiResult<NantyTelegram>> {
  return request<unknown>({ method: "GET", path: "/api/nanty/telegram" }).then(parsed(parseNantyTelegram));
}

/** Grava o token do bot (vazio remove). A resposta traz só a máscara. */
export function postNantyToken(token: string): Promise<ApiResult<NantyTelegram>> {
  return request<unknown>({ method: "POST", path: "/api/nanty/telegram/token", body: JSON.stringify({ token }) }).then(parsed(parseNantyTelegram));
}

/** Parear (código novo), desparear ou a mensagem de teste. */
export function postNantyTelegram(action: "pair" | "unpair" | "test"): Promise<ApiResult<NantyTelegram>> {
  return request<unknown>({ method: "POST", path: `/api/nanty/telegram/${action}` }).then(parsed(parseNantyTelegram));
}
