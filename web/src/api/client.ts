/*
 * Cliente HTTP da SPA. Regras:
 * - Nunca lança exceção para erro de servidor/rede: devolve `ApiResult`.
 * - Erros são estáticos: `message` vem de um catálogo fixo (ou de um código fixo
 *   conhecido do servidor). O corpo cru da resposta nunca é exposto.
 * - Chaves de API (`api_key`) só entram no corpo de putConnection/testConnection;
 *   nunca são logadas, guardadas nem devolvidas pelo cliente.
 */
import type {
  ActivitySnapshot,
  Agents,
  ApproveResult,
  RecoveryChoiceBody,
  RecoveryChoiceResult,
  BrowseView,
  ChatAttachmentUpload,
  ChatHistoryTurn,
  ChatHistoryView,
  ChatReply,
  ConfigView,
  NetworkView,
  NetworkSaveResult,
  ModelPricing,
  SpecBudget,
  UsageView,
  DialogMessage,
  DialogView,
  ToolActionItem,
  ToolActionsView,
  ChatgptStartResult,
  ConnectionInput,
  ConnectionView,
  ModelsResult,
  PrereqItem,
  PermissionDecision,
  ResolveResult,
  Project,
  ProjectState,
  ResultCommit,
  ResultFile,
  ResultReadme,
  ResultTask,
  SpecReport,
  SpecResult,
  SpecStatus,
  TaskStatus,
  TestConnectionInput,
  TestConnectionResult,
} from "./types";
import {
  isProviderFailureCode,
  localizeProviderDetail,
  prereqMissingMessage,
  providerFailureDetail,
  providerFailureMessage,
  providerReasonText,
} from "../copy";
import { isAttempt, isSafeSeq } from "./events";
import { parseNetworkView } from "../desktop/network";
import { isAttachmentName, MAX_CHAT_ATTACHMENTS } from "../chatImages";
// Direto do dicionário pt-BR (sem dependências): o índice do i18n importa o estado da interface,
// que importa este cliente.
import { ptBR, type MessageKey } from "../i18n/ptBR";
import { parseCodeParams, parseCoded, isMessageCode, type CodeParams } from "./codes";

type ApiErrorKind = "network" | "timeout" | "http" | "invalid_response";

/** Erro estático e seguro para exibir ao usuário. */
export interface ApiFailure {
  kind: ApiErrorKind;
  /** Status HTTP; 0 quando não houve resposta. */
  status: number;
  message: string;
  /** Código fixo do servidor (ex.: `base_dirty` em 409 de aprovação). Só valores conhecidos. */
  reason?: string;
  /** Causa fixa do provedor (`provider_http_400`, `provider_timeout`...). Só valores do vocabulário. */
  providerCode?: string;
  /** Motivo curto da falha do provedor (`detail` do 502 do chat, spec 088). Só texto validado. */
  providerDetail?: string;
  /**
   * Motivo neutro da falha do provedor (spec 119): `params.kind` e `params.provider_code` do 502
   * do chat. A tela monta o rótulo pelo dicionário, no idioma ativo.
   */
  providerKind?: string;
  providerKindCode?: string;
  /** Detalhe técnico (já redigido pelo servidor) de falhas do app de teste; exibido recolhido. */
  detail?: string;
  /** Ids das pendências do 412 `prereq_missing` (spec 093 T02); só ids no formato fixo. */
  missing?: string[];
  /**
   * Código estável do servidor (spec 102), ex.: `host_forbidden`; a interface o traduz pelo
   * dicionário (`serverError.<code>`) no idioma ativo. Só no formato fixo.
   */
  code?: string;
  /** Parâmetros do `code` (textos e números). */
  params?: CodeParams;
  /**
   * Chave do texto de reserva do cliente (status HTTP, rede, tempo limite), quando `message` é um
   * deles: resolvida no idioma ativo na hora de exibir (`apiFailureText`). `message` fica em pt-BR.
   */
  messageKey?: MessageKey;
}

export type ApiResult<T> = { ok: true; data: T } | { ok: false; error: ApiFailure };

/** Timeout padrão de chamadas comuns. */
const DEFAULT_TIMEOUT_MS = 10_000;
/** Start/stop do app podem demorar (build, parada do container). */
const APP_LIFECYCLE_TIMEOUT_MS = 60_000;
/** Teste de conexão: o servidor limita a 15 s, então o cliente espera um pouco mais. */
const TEST_CONNECTION_TIMEOUT_MS = 20_000;
/** Turno do Arquiteto: o servidor limita a 180 s (chat.rs `CHAT_TIMEOUT`), então 200 s com margem. */
const CHAT_TIMEOUT_MS = 16 * 60_000;
/** Listagem de modelos: mesmo teto de espera do teste de conexão, com margem para o servidor. */
const MODELS_TIMEOUT_MS = 20_000;

/** Chaves dos textos de reserva por código HTTP (usados quando o servidor não traz texto conhecido). */
const STATUS_KEYS: Record<number, MessageKey> = {
  400: "api.status.400",
  403: "api.status.403",
  404: "api.status.404",
  409: "api.status.409",
  413: "api.status.413",
  422: "api.status.422",
};

/**
 * Reservas do cliente (spec 102 e 115): o texto pt-BR em `message` (registros e compatibilidade) e a
 * chave em `messageKey`, que a interface mostra no idioma ativo (`apiFailureText`) quando o servidor
 * não manda `code`. O texto do servidor nunca é repassado: os erros dele vão pelo `code` (115 T02).
 */
const GENERIC_HTTP_KEY: MessageKey = "api.genericHttp";
const SERVER_ERROR_KEY: MessageKey = "api.serverError";
const NETWORK_KEY: MessageKey = "api.network";
const TIMEOUT_KEY: MessageKey = "api.timeout";
const INVALID_RESPONSE_KEY: MessageKey = "api.invalidResponse";
const SERVER_ERROR_MESSAGE = ptBR[SERVER_ERROR_KEY];

/** Reserva do cliente: o texto pt-BR (`message`) e a chave (`messageKey`) para o idioma ativo. */
export function clientReserve(key: MessageKey): { message: string; messageKey: MessageKey } {
  return { message: ptBR[key], messageKey: key };
}

/** Completa a falha (spec 102): `code`/`params` do corpo, quando no formato fixo. */
function withCodes(failure: ApiFailure, body: unknown): ApiFailure {
  const out: ApiFailure = { ...failure };
  if (isRecord(body) && isMessageCode(body.code)) {
    out.code = body.code;
    const params = parseCodeParams(body.params);
    if (params !== null) out.params = params;
  }
  return out;
}

/** Código do erro da API no corpo (`code`, spec 115 T02), ou `null`. */
export function serverErrorCode(body: unknown): string | null {
  return isRecord(body) && isMessageCode(body.code) ? body.code : null;
}

/** Códigos `reason` fixos devolvidos pelo servidor em respostas de erro (runs.rs). */
const KNOWN_REASONS: ReadonlySet<string> = new Set([
  "no_tasks",
  "invalid_task",
  "dependency_missing",
  "dependency_cycle",
  "test_command_missing",
  "base_branch_missing",
  "settings_invalid",
  "not_git_repo",
  "base_dirty",
  "report_in_progress",
  "draft_saved",
  "no_active_task",
  "budget_exceeded",
  "environment_not_docker",
  "environment_invalid",
  "environment_changed",
  "docker_unavailable",
  "architect_turn_active",
  "plan_without_tasks",
  // Aprovação durante a revisão do plano (spec 128 T04).
  "plan_review_running",
  // 502 do chat com a queda da conexão gravada no histórico (spec 127 T01).
  "connection_lost",
  "prereq_missing",
]);

/**
 * Formato de um id de pendência: `git`, `docker_daemon` ou com parâmetro (T12), como
 * `connection_key:<id>`, `connection_url:<id>` e `pricing:<modelo>` (ASCII visível, sem espaço).
 */
const PREREQ_ID = /^[a-z_]{1,32}(?::[\x21-\x7e]{1,127})?$/;

/** Ids do 412 `{error: "prereq_missing", missing: [...]}`; `null` se o corpo não for esse. */
function prereqMissingIds(body: unknown): string[] | null {
  if (!isRecord(body) || body.error !== "prereq_missing" || !Array.isArray(body.missing)) return null;
  return body.missing.filter((id): id is string => typeof id === "string" && PREREQ_ID.test(id));
}

/** Extrai `reason` do corpo somente se for um código conhecido. */
function serverReason(body: unknown): string | null {
  if (typeof body !== "object" || body === null || Array.isArray(body)) return null;
  const reason = (body as { reason?: unknown }).reason;
  return typeof reason === "string" && KNOWN_REASONS.has(reason) ? reason : null;
}

/** Códigos do 422 de agente sem configuração (runs.rs e chat.rs). */
const AGENT_NOT_CONFIGURED_CODES: ReadonlySet<string> = new Set([
  "architect_not_configured",
  "developer_not_configured",
  "reviewer_not_configured",
]);

/** `true` só para o 422 de agente não configurado (não para outros 422). */
export function isAgentNotConfigured(failure: ApiFailure): boolean {
  return failure.status === 422 && failure.code !== undefined && AGENT_NOT_CONFIGURED_CODES.has(failure.code);
}

interface RequestOptions {
  timeoutMs?: number;
}

interface RawRequest {
  method: "GET" | "PUT" | "POST" | "PATCH" | "DELETE";
  path: string;
  /** Corpo JSON já serializado (somente mutações). */
  body?: string;
  timeoutMs?: number;
  /** Mapeamento específico de um endpoint para erros HTTP; `null` segue o padrão. */
  onHttpError?: (status: number, body: unknown) => ApiFailure | null;
  /** Cabeçalhos extras de requisição (ex.: `If-None-Match`). */
  headers?: Record<string, string>;
  /** Observa a resposta antes da leitura do corpo (ex.: para ler o `ETag`). */
  onResponse?: (response: Response) => void;
  /** Cancelamento pelo chamador; abortar vira falha `timeout` (status 0), como o prazo. */
  signal?: AbortSignal;
  /** Corpo binário (upload de imagem) com o seu `Content-Type`; substitui o corpo JSON. */
  binary?: { body: Blob; contentType: string };
  /** `fetch` com `keepalive` (sobrevive ao descarregamento da página; corpo pequeno). */
  keepalive?: boolean;
}

/** Chave da reserva do cliente para o status HTTP. */
function statusKey(status: number): MessageKey {
  if (status >= 500) return SERVER_ERROR_KEY;
  return STATUS_KEYS[status] ?? GENERIC_HTTP_KEY;
}


function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Idioma ativo da interface enviado ao servidor em toda requisição (spec 102, decisão 3): o servidor
 * o guarda nas que mudam algo (turno do Arquiteto, execução) e os agentes escrevem nele. Quem o
 * mantém é o i18n (`setLanguage`); `null` = não envia (o servidor segue com o último informado).
 */
export const LANG_HEADER = "X-Nants-Lang";

/** Página de entrada do aparelho sem autorização (spec 112). */
export const ENTRY_PATH = "/connect";

/** Tamanho máximo do caminho de volta (`next`), em bytes (o mesmo do servidor). */
export const MAX_NEXT_BYTES = 2048;

/**
 * Caminho de volta aceito em `next` (spec 128), com a mesma regra do servidor (`safe_next`, em
 * `src/server/devices.rs`): só caminho relativo do próprio Nants. Começa com `/` e não com `//`,
 * sem `\`, sem espaço nem caractere de controle, com no máximo {@link MAX_NEXT_BYTES} e diferente
 * da própria entrada. Aceita `?query` e `#hash`. Qualquer outra coisa vira `null` (sem
 * redirecionamento aberto).
 */
export function safeNext(raw: string | null | undefined): string | null {
  if (typeof raw !== "string" || raw.length === 0) return null;
  if (new TextEncoder().encode(raw).length > MAX_NEXT_BYTES) return null;
  if (!raw.startsWith("/") || raw.startsWith("//") || raw.includes("\\")) return null;
  if (/[\s\u0000-\u001f\u007f-\u009f]/u.test(raw)) return null;
  const path = raw.split(/[?#]/u, 1)[0];
  if (path === ENTRY_PATH) return null;
  return raw;
}

/** Endereço da entrada com o caminho de volta (percent-encodado), se ele for aceito e não for `/`. */
export function entryLocation(next: string | null | undefined): string {
  const safe = safeNext(next);
  return safe === null || safe === "/" ? ENTRY_PATH : `${ENTRY_PATH}?next=${encodeURIComponent(safe)}`;
}

/**
 * Aparelho sem autorização (spec 112): com a rede local ligada, a API responde 401
 * `device_required` (cookie perdido ou dispositivo revogado). A interface vai para a página de
 * entrada, que pede o código, levando a página atual (`pathname + search + hash`) como `next`
 * (spec 128): depois de entrar, o aparelho volta para ela. Substituível nos testes.
 */
export const deviceAuth = {
  /** Página atual (`pathname`, `search`, `hash`); `null` fora do navegador. */
  here(): Pick<Location, "pathname" | "search" | "hash"> | null {
    return typeof window === "undefined" ? null : window.location;
  },
  /** Navega para a entrada. */
  go(to: string): void {
    window.location.assign(to);
  },
  onDeviceRequired(): void {
    const here = deviceAuth.here();
    if (here === null || here.pathname === ENTRY_PATH) return;
    deviceAuth.go(entryLocation(`${here.pathname}${here.search}${here.hash}`));
  },
};

function isDeviceRequired(status: number, body: unknown): boolean {
  return status === 401 && isRecord(body) && body.code === "device_required";
}
let requestLanguage: "pt-BR" | "en" | null = null;

/** Define o idioma enviado em `X-Nants-Lang` (chamado pelo i18n a cada troca). */
export function setRequestLanguage(lang: "pt-BR" | "en" | null): void {
  requestLanguage = lang;
}

/** Idioma enviado hoje em `X-Nants-Lang` (testes). */
export function getRequestLanguage(): "pt-BR" | "en" | null {
  return requestLanguage;
}

/**
 * Executa a requisição. Único ponto de `fetch` do cliente.
 * Nenhum erro escapa: falhas viram `ApiFailure`.
 */
export async function request<T>(
  raw: RawRequest,
  fetchImpl: typeof fetch = fetch,
): Promise<ApiResult<T>> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), raw.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  if (raw.signal !== undefined) {
    if (raw.signal.aborted) controller.abort();
    else raw.signal.addEventListener("abort", () => controller.abort(), { once: true });
  }
  const headers: Record<string, string> = { Accept: "application/json", ...raw.headers };
  if (requestLanguage !== null) headers[LANG_HEADER] = requestLanguage;
  // O servidor exige JSON em POST/PUT/DELETE: sem corpo, envia `{}` (senão 415).
  const noBody = raw.method === "GET";
  let payload: string | Blob | undefined = noBody ? undefined : (raw.body ?? "{}");
  if (payload !== undefined) headers["Content-Type"] = "application/json";
  if (!noBody && raw.binary !== undefined) {
    payload = raw.binary.body;
    headers["Content-Type"] = raw.binary.contentType;
  }

  let response: Response;
  try {
    response = await fetchImpl(raw.path, {
      method: raw.method,
      headers,
      body: payload,
      credentials: "same-origin",
      signal: controller.signal,
      ...(raw.keepalive === true ? { keepalive: true } : {}),
    });
  } catch (err: unknown) {
    clearTimeout(timer);
    const aborted = err instanceof DOMException && err.name === "AbortError";
    return aborted
      ? fail("timeout", 0, TIMEOUT_KEY)
      : fail("network", 0, NETWORK_KEY);
  }

  try {
    raw.onResponse?.(response);
    if (!response.ok) {
      const body = await safeJson(response);
      if (isDeviceRequired(response.status, body)) deviceAuth.onDeviceRequired();
      const special = raw.onHttpError?.(response.status, body) ?? null;
      if (special !== null) return { ok: false, error: withCodes(special, body) };
      const missing = response.status === 412 ? prereqMissingIds(body) : null;
      if (missing !== null) {
        const message = prereqMissingMessage(missing);
        return { ok: false, error: withCodes({ kind: "http", status: 412, message, reason: "prereq_missing", missing }, body) };
      }
      const reason = serverReason(body);
      const reserve = clientReserve(statusKey(response.status));
      const failure: ApiFailure =
        reason === null ? { kind: "http", status: response.status, ...reserve } : { kind: "http", status: response.status, ...reserve, reason };
      return { ok: false, error: withCodes(failure, body) };
    }
    const body = await safeJson(response);
    if (body === PARSE_FAILED) {
      return fail("invalid_response", response.status, INVALID_RESPONSE_KEY);
    }
    return { ok: true, data: body as T };
  } catch {
    // Corpo interrompido pelo timeout durante a leitura.
    return controller.signal.aborted
      ? fail("timeout", 0, TIMEOUT_KEY)
      : fail("network", 0, NETWORK_KEY);
  } finally {
    clearTimeout(timer);
  }
}

const PARSE_FAILED = Symbol("parse-failed");

/** Lê JSON sem lançar: corpo vazio vira `{}`; JSON inválido vira PARSE_FAILED. */
async function safeJson(response: Response): Promise<unknown> {
  const text = await response.text();
  if (text.length === 0) return {};
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return PARSE_FAILED;
  }
}

function fail<T>(kind: ApiErrorKind, status: number, key: MessageKey): ApiResult<T> {
  return { ok: false, error: { kind, status, ...clientReserve(key) } };
}

function enc(segment: string): string {
  return encodeURIComponent(segment);
}

/** Dispensa leitura cega: valida só que a resposta é um objeto (ou repassa o tipo). */
function asObject<T>(result: ApiResult<unknown>): ApiResult<T> {
  if (!result.ok) return result;
  if (!isRecord(result.data)) return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
  return { ok: true, data: result.data as T };
}

// ---------- Endpoints ----------

/** GET /api/system/prereqs */
export function getPrereqs(opts?: RequestOptions): Promise<ApiResult<PrereqItem[]>> {
  return request<PrereqItem[]>({ method: "GET", path: "/api/system/prereqs", timeoutMs: opts?.timeoutMs }).then(
    (r) => {
      if (!r.ok) return r;
      if (!Array.isArray(r.data)) return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
      return r;
    },
  );
}

/** GET /api/config */
export function getConfig(opts?: RequestOptions): Promise<ApiResult<ConfigView>> {
  return request<ConfigView>({ method: "GET", path: "/api/config", timeoutMs: opts?.timeoutMs }).then(
    (r) => asObject<ConfigView>(r),
  );
}

/** GET /api/config/network (spec 111): validado por `parseNetworkConfig` em quem exibe. */
export function getNetworkConfig(opts?: RequestOptions): Promise<ApiResult<unknown>> {
  return request<unknown>({ method: "GET", path: "/api/config/network", timeoutMs: opts?.timeoutMs }).then((r) => asObject<unknown>(r));
}

/** PUT /api/config/network `{lan}`: grava a opção salva; vale a partir do próximo início. */
export function putNetworkConfig(lan: boolean): Promise<ApiResult<NetworkSaveResult>> {
  return request<NetworkSaveResult>({
    method: "PUT",
    path: "/api/config/network",
    body: JSON.stringify({ lan }),
  }).then((r) => asObject<NetworkSaveResult>(r));
}

/**
 * PUT /api/config/connections (upsert por id).
 * `input.api_key` trafega só neste corpo; o chamador deve descartá-la após o envio.
 */
export function putConnection(input: ConnectionInput): Promise<ApiResult<ConnectionView>> {
  return request<ConnectionView>({
    method: "PUT",
    path: "/api/config/connections",
    body: JSON.stringify(input),
  }).then((r) => asObject<ConnectionView>(r));
}

/**
 * POST /api/connections/chatgpt/start (spec 116): começa o login pelo plano ChatGPT. `connection_id`
 * reconecta uma conexão salva; sem ele, o login cria uma conexão nova. Devolve a URL de autorização
 * e se o pedido veio deste computador (`local`).
 */
export function startChatgptLogin(input: { name?: string; connection_id?: string }): Promise<ApiResult<ChatgptStartResult>> {
  return request<ChatgptStartResult>({
    method: "POST",
    path: "/api/connections/chatgpt/start",
    body: JSON.stringify(input),
  }).then((r) => {
    const result = asObject<ChatgptStartResult>(r);
    if (!result.ok) return result;
    const { url, local } = result.data;
    return typeof url === "string" && url.startsWith("https://") && typeof local === "boolean"
      ? { ok: true, data: { url, local } }
      : fail("invalid_response", 200, INVALID_RESPONSE_KEY);
  });
}

/**
 * POST /api/connections/:id/chatgpt/logout (spec 116): revoga e apaga as credenciais locais. O
 * servidor responde `{state, revoked}`; `revoked` fora do formato conta como não confirmado (a tela
 * avisa: melhor um aviso a mais do que dar a revogação por feita).
 */
export function logoutChatgpt(id: string): Promise<ApiResult<{ state: "disconnected"; revoked: boolean }>> {
  return request<unknown>({
    method: "POST",
    path: `/api/connections/${enc(id)}/chatgpt/logout`,
  }).then((r) => {
    if (!r.ok) return r;
    const revoked = isRecord(r.data) && r.data.revoked === true;
    return { ok: true, data: { state: "disconnected", revoked } };
  });
}

/** PUT /api/config/agents (substitui os três papéis). */
export function putAgents(agents: Agents): Promise<ApiResult<Agents>> {
  return request<Agents>({
    method: "PUT",
    path: "/api/config/agents",
    body: JSON.stringify(agents),
  }).then((r) => asObject<Agents>(r));
}

/** PUT /api/config/pricing (substitui o mapa modelo -> preço). */
export function putPricing(pricing: Record<string, ModelPricing>): Promise<ApiResult<Record<string, ModelPricing>>> {
  return request<Record<string, ModelPricing>>({
    method: "PUT",
    path: "/api/config/pricing",
    body: JSON.stringify(pricing),
  }).then((r) => asObject<Record<string, ModelPricing>>(r));
}

/** GET /api/config/pricing/estimate?models=a,b (estimativas do catálogo; não grava). */
export function getPriceEstimate(models: string[]): Promise<ApiResult<Record<string, ModelPricing>>> {
  return request<Record<string, ModelPricing>>({
    method: "GET",
    path: `/api/config/pricing/estimate?models=${encodeURIComponent(models.join(","))}`,
    timeoutMs: 30_000,
  }).then((r) => asObject<Record<string, ModelPricing>>(r));
}

/** POST /api/config/pricing/autofill: grava estimativas dos modelos em uso (nunca sobrescreve manual). */
export function autofillPricing(refresh = false): Promise<ApiResult<Record<string, ModelPricing>>> {
  return request<Record<string, ModelPricing>>({
    method: "POST",
    path: `/api/config/pricing/autofill${refresh ? "?refresh=true" : ""}`,
    body: "{}",
    timeoutMs: 30_000,
  }).then((r) => asObject<Record<string, ModelPricing>>(r));
}

/** GET /api/projects/:id/usage?spec=<id> */
export function getUsage(projectId: string, specId?: string): Promise<ApiResult<UsageView>> {
  return request<UsageView>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/usage${specId === undefined ? "" : `?spec=${enc(specId)}`}`,
  }).then((r) => asObject<UsageView>(r));
}

/** PUT /api/projects/:id/specs/:spec/budget (`{}` remove o limite). */
export function putBudget(projectId: string, specId: string, budget: SpecBudget): Promise<ApiResult<SpecBudget>> {
  return request<SpecBudget>({
    method: "PUT",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/budget`,
    body: JSON.stringify(budget),
  }).then((r) => asObject<SpecBudget>(r));
}

/** POST /api/config/connections/test. `api_key` trafega só neste corpo. */
export function testConnection(
  input: TestConnectionInput,
): Promise<ApiResult<TestConnectionResult>> {
  return request<TestConnectionResult>({
    method: "POST",
    path: "/api/config/connections/test",
    body: JSON.stringify(input),
    timeoutMs: TEST_CONNECTION_TIMEOUT_MS,
  }).then((r) => asObject<TestConnectionResult>(r));
}

/**
 * GET /api/config/connections/:id/models (lista da conexão SALVA; `refresh` ignora o cache do servidor).
 * Falha do provedor chega como `ok: true` com `error` (código fixo); aqui só se valida o formato.
 */
export function getConnectionModels(
  id: string,
  opts?: { refresh?: boolean },
): Promise<ApiResult<ModelsResult>> {
  const query = opts?.refresh === true ? "?refresh=true" : "";
  return request<ModelsResult>({
    method: "GET",
    path: `/api/config/connections/${enc(id)}/models${query}`,
    timeoutMs: MODELS_TIMEOUT_MS,
  }).then((r) => {
    const result = asObject<ModelsResult>(r);
    if (!result.ok) return result;
    return Array.isArray(result.data.models)
      ? result
      : fail("invalid_response", 200, INVALID_RESPONSE_KEY);
  });
}

/**
 * POST /api/projects. O 409 de pasta dentro de outro repositório vem com `code`
 * `project_nested_repo` e `params.path` (o repositório), traduzido pelo código.
 */
export function createProject(input: { name: string; path: string }): Promise<ApiResult<Project>> {
  return request<Project>({
    method: "POST",
    path: "/api/projects",
    body: JSON.stringify({ name: input.name, path: input.path }),
  }).then((r) => asObject<Project>(r));
}

/** GET /api/projects/:id/state */
export function getProjectState(projectId: string): Promise<ApiResult<ProjectState>> {
  return request<ProjectState>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/state`,
  }).then((r) => asObject<ProjectState>(r));
}

/** PUT /api/projects/:id/settings com os limites (1–10); responde o estado do projeto. */
export function putProjectLimits(
  projectId: string,
  limits: { max_attempts: number; max_test_cycles: number },
): Promise<ApiResult<ProjectState>> {
  return request<ProjectState>({
    method: "PUT",
    path: `/api/projects/${enc(projectId)}/settings`,
    body: JSON.stringify(limits),
  }).then((r) => asObject<ProjectState>(r));
}

/** PUT /api/projects/:id/yolo: liga/desliga o Modo YOLO; responde o estado do projeto. */
export function putProjectYolo(projectId: string, enabled: boolean): Promise<ApiResult<ProjectState>> {
  return request<ProjectState>({
    method: "PUT",
    path: `/api/projects/${enc(projectId)}/yolo`,
    body: JSON.stringify({ enabled }),
  }).then((r) => asObject<ProjectState>(r));
}

/** PUT /api/projects/:id/internet: liga/desliga a internet do Arquiteto; responde o estado do projeto. */
export function putProjectInternet(projectId: string, enabled: boolean): Promise<ApiResult<ProjectState>> {
  return request<ProjectState>({
    method: "PUT",
    path: `/api/projects/${enc(projectId)}/internet`,
    body: JSON.stringify({ enabled }),
  }).then((r) => asObject<ProjectState>(r));
}

/** GET /api/projects/:id/activity: itens malformados são descartados. */
export function getProjectActivity(projectId: string): Promise<ApiResult<ActivitySnapshot[]>> {
  return request<{ activities?: unknown }>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/activity`,
  }).then((r) => {
    const result = asObject<{ activities?: unknown }>(r);
    if (!result.ok) return result;
    const raw = result.data.activities;
    if (!Array.isArray(raw)) return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
    const items: ActivitySnapshot[] = [];
    for (const it of raw as unknown[]) {
      if (!isRecord(it) || typeof it.agent !== "string" || typeof it.activity_id !== "string") continue;
      const str = (v: unknown): string => (typeof v === "string" ? v : "");
      const nul = (v: unknown): string | null => (typeof v === "string" ? v : null);
      const age = typeof it.age_ms === "number" && Number.isFinite(it.age_ms) ? Math.max(0, it.age_ms) : 0;
      items.push({
        agent: it.agent,
        task_id: nul(it.task_id),
        spec_id: nul(it.spec_id),
        activity_id: it.activity_id,
        reasoning: str(it.reasoning),
        text: str(it.text),
        tool: str(it.tool),
        command: nul(it.command),
        command_started_at:
          typeof it.command_started_at === "number" && Number.isFinite(it.command_started_at) ? it.command_started_at : null,
        // Spec 114 T04: chamada ao modelo em curso (início e bytes recebidos).
        ...(typeof it.model_started_at === "number" && Number.isFinite(it.model_started_at) ? { model_started_at: it.model_started_at } : {}),
        ...(typeof it.received_bytes === "number" && Number.isSafeInteger(it.received_bytes) && it.received_bytes >= 0 ? { received_bytes: it.received_bytes } : {}),
        stage: typeof it.stage === "string" && it.stage !== "" ? it.stage : null,
        ...codedFields(it),
        ...(typeof it.structured === "string" && it.structured !== "" ? { structured: it.structured } : {}),
        // Spec 123 T01: progresso real do comando em curso e os `params` da etapa.
        ...(typeof it.output_bytes === "number" && Number.isSafeInteger(it.output_bytes) && it.output_bytes >= 0 ? { output_bytes: it.output_bytes } : {}),
        ...(typeof it.last_line === "string" && it.last_line.trim() !== "" ? { last_line: it.last_line } : {}),
        // Spec 123 T02: limite de tempo do comando em curso.
        ...(typeof it.command_limit_ms === "number" && Number.isSafeInteger(it.command_limit_ms) && it.command_limit_ms > 0 ? { command_limit_ms: it.command_limit_ms } : {}),
        ...(isMessageCode(it.stage_code) ? stageParams(it.stage_params) : {}),
        age_ms: age,
      });
    }
    return { ok: true, data: items };
  });
}

const SPEC_STATUSES: ReadonlySet<string> = new Set([
  "DRAFT",
  "AWAITING_APPROVAL",
  "IN_PROGRESS",
  "COMPLETED",
  "BLOCKED",
]);

const TASK_STATUSES: ReadonlySet<string> = new Set([
  "TODO",
  "IN_PROGRESS",
  "IN_REVIEW",
  "CHANGES_REQUESTED",
  "APPROVED",
  "INTEGRATING",
  "DONE",
  "BLOCKED",
]);

const FILE_CHANGES: ReadonlySet<string> = new Set(["A", "M", "D"]);

const isString = (value: unknown): value is string => typeof value === "string";
const isNullableString = (value: unknown): boolean => value === null || isString(value);

/** Valida um item de `tasks`. `error` é opcional (ausente quando não há). */
function toResultTask(value: unknown): ResultTask | null {
  if (!isRecord(value) || !isString(value.id) || !isString(value.title)) return null;
  const status = value.status;
  if (status !== null && !(isString(status) && TASK_STATUSES.has(status))) return null;
  if (value.error !== undefined && !isString(value.error)) return null;
  const task: ResultTask = { id: value.id, title: value.title, status: status as TaskStatus | null };
  if (isString(value.error)) task.error = value.error;
  return task;
}

function toResultCommit(value: unknown): ResultCommit | null {
  if (!isRecord(value)) return null;
  const { hash, subject, task_id: taskId, date } = value;
  if (!isString(hash) || !isString(subject) || !isString(taskId) || !isString(date)) return null;
  return { hash, subject, task_id: taskId, date };
}

function toResultFile(value: unknown): ResultFile | null {
  if (!isRecord(value) || !isString(value.path)) return null;
  if (!isString(value.change) || !FILE_CHANGES.has(value.change)) return null;
  return { path: value.path, change: value.change as ResultFile["change"] };
}

function toResultReadme(value: unknown): ResultReadme | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !isString(value.text) || typeof value.truncated !== "boolean") return undefined;
  return { text: value.text, truncated: value.truncated };
}

/** Lista validada item a item; qualquer item fora do formato invalida a resposta inteira. */
function allOrNothing<T>(raw: unknown, convert: (item: unknown) => T | null): T[] | null {
  if (!Array.isArray(raw)) return null;
  const items: T[] = [];
  for (const item of raw) {
    const converted = convert(item);
    if (converted === null) return null;
    items.push(converted);
  }
  return items;
}

/**
 * Valida o payload de `GET .../result` campo a campo. Devolve `null` quando o formato não
 * bate com o contrato (o chamador trata como resposta inválida, sem quebrar a tela).
 */
function toSpecResult(value: unknown): SpecResult | null {
  if (!isRecord(value)) return null;
  const status = value.status;
  if (status !== null && !(isString(status) && SPEC_STATUSES.has(status))) return null;
  const tasks = allOrNothing(value.tasks, toResultTask);
  const commits = allOrNothing(value.commits, toResultCommit);
  const files = allOrNothing(value.files, toResultFile);
  const readme = toResultReadme(value.readme);
  if (
    !isString(value.spec_id) ||
    !isString(value.title) ||
    tasks === null ||
    commits === null ||
    files === null ||
    typeof value.files_truncated !== "boolean" ||
    readme === undefined ||
    !isString(value.project_path) ||
    !isNullableString(value.test_command)
  ) {
    return null;
  }
  return {
    spec_id: value.spec_id,
    title: value.title,
    status: status as SpecStatus | null,
    tasks,
    commits,
    files,
    files_truncated: value.files_truncated,
    readme,
    project_path: value.project_path,
    test_command: value.test_command as string | null,
    run_command: isString(value.run_command) && value.run_command.trim() !== "" ? value.run_command : null,
    run_url: isString(value.run_url) && value.run_url !== "" ? value.run_url : null,
    environment: value.environment === "docker" ? "docker" : "host",
  };
}

function toSpecReport(value: unknown): SpecReport | null {
  if (!isRecord(value)) return null;
  if (!isString(value.markdown) || !isString(value.generated_at) || !isString(value.source)) return null;
  return { markdown: value.markdown, generated_at: value.generated_at, source: value.source };
}

/**
 * GET /api/projects/:id/specs/:spec_id/report: relatório do Arquiteto. 404 (sem relatório) é falha
 * de HTTP com `status` 404; quem chama decide o fallback.
 */
export function getSpecReport(projectId: string, specId: string): Promise<ApiResult<SpecReport>> {
  return request<SpecReport>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/report`,
  }).then((r) => {
    if (!r.ok) return r;
    const parsed = toSpecReport(r.data);
    return parsed === null ? fail("invalid_response", 200, INVALID_RESPONSE_KEY) : { ok: true, data: parsed };
  });
}

/** GET /api/projects/:id/specs/:spec_id/result: resultado somente leitura de uma spec. */
export function getSpecResult(projectId: string, specId: string): Promise<ApiResult<SpecResult>> {
  return request<SpecResult>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/result`,
  }).then((r) => {
    if (!r.ok) return r;
    const parsed = toSpecResult(r.data);
    return parsed === null ? fail("invalid_response", 200, INVALID_RESPONSE_KEY) : { ok: true, data: parsed };
  });
}

/**
 * GET /api/projects/:id/chat: histórico do Arquiteto. Só entram turnos com role, text e ts válidos.
 * Falha de rede/HTTP segue o padrão do cliente (o chamador decide o que mostrar).
 */
export function getChatHistory(projectId: string): Promise<ApiResult<ChatHistoryView>> {
  return request<ChatHistoryView>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/chat`,
  }).then((r) => {
    const result = asObject<ChatHistoryView>(r);
    if (!result.ok) return result;
    const raw: unknown = result.data.turns;
    if (!Array.isArray(raw)) return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
    const turns = raw.flatMap((item) => {
      const turn = toChatHistoryTurn(item);
      return turn === null ? [] : [turn];
    });
    return { ok: true, data: { turns } };
  });
}

/** Identidade de turno aceita: hex minúsculo de 1 a 64 caracteres. Qualquer outra vira ausente. */
const TURN_ID_PATTERN = /^[0-9a-f]{1,64}$/;

/** Turno válido do histórico; `null` descarta só o item. Um `turn_id` inválido é removido, não o turno. */
function toChatHistoryTurn(value: unknown): ChatHistoryTurn | null {
  if (!isRecord(value)) return null;
  const role = value.role;
  if (!((role === "user" || role === "architect" || role === "system") && typeof value.text === "string")) return null;
  if (typeof value.ts !== "number" || !Number.isFinite(value.ts)) return null;
  const turn: ChatHistoryTurn = { role, text: value.text, ts: value.ts };
  // Aviso do sistema (spec 127 T01): `code` no formato fixo e `params` saneados.
  if (role === "system") {
    const coded = parseCoded(value.code, value.params);
    if (coded !== undefined) {
      turn.code = coded.code;
      turn.params = coded.params;
    }
  }
  if (typeof value.turn_id === "string" && TURN_ID_PATTERN.test(value.turn_id)) {
    turn.turn_id = value.turn_id;
  }
  if (Array.isArray(value.attachments)) {
    const names = value.attachments.filter((n): n is string => typeof n === "string" && isAttachmentName(n));
    if (names.length > 0) turn.attachments = names.slice(0, MAX_CHAT_ATTACHMENTS);
  }
  return turn;
}

/** Mensagens fixas das falhas do upload de imagem. */
export const ATTACHMENT_TOO_LARGE = ptBR["api.attachmentTooLarge"];
export const ATTACHMENT_UNSUPPORTED = ptBR["api.attachmentUnsupported"];
export const ATTACHMENT_NO_SPACE = ptBR["api.attachmentNoSpace"];

/** Prazo do upload de uma imagem (rede lenta no celular). */
const ATTACHMENT_TIMEOUT_MS = 60_000;

/**
 * POST /api/projects/:id/chat/attachments: corpo = bytes da imagem (`image/png`, `image/jpeg` ou
 * `image/webp`). Devolve `{id, name}`; 413/415/507 viram mensagens próprias.
 */
export function uploadChatAttachment(
  projectId: string,
  image: Blob,
  contentType: string,
): Promise<ApiResult<ChatAttachmentUpload>> {
  return request<ChatAttachmentUpload>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/chat/attachments`,
    binary: { body: image, contentType },
    timeoutMs: ATTACHMENT_TIMEOUT_MS,
    onHttpError: (status) => {
      const key: MessageKey | null =
        status === 413 ? "api.attachmentTooLarge" : status === 415 ? "api.attachmentUnsupported" : status === 507 ? "api.attachmentNoSpace" : null;
      return key === null ? null : { kind: "http", status, ...clientReserve(key) };
    },
  }).then((r) => {
    const result = asObject<ChatAttachmentUpload>(r);
    if (!result.ok) return result;
    const { id, name } = result.data;
    if (typeof id !== "string" || typeof name !== "string" || !isAttachmentName(name)) {
      return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
    }
    return { ok: true, data: { id, name } };
  });
}

/** Parâmetros de `getDialog`: `after` é o seq exclusivo; `limit` vai de 1 a 500 (padrão do servidor: 200). */
interface DialogQuery {
  after?: number;
  limit?: number;
}

/** Mensagem do diálogo: todos os campos com tipo conferido; `task_id` e `attempt` ausentes viram `null`. */
function toDialogMessage(value: unknown): DialogMessage | null {
  if (!isRecord(value)) return null;
  const { seq, ts, agent, text } = value;
  if (!isSafeSeq(seq) || typeof ts !== "number" || !Number.isFinite(ts)) return null;
  if (!isString(agent) || !isString(text)) return null;
  const taskId = value.task_id ?? null;
  const attempt = value.attempt ?? null;
  if (taskId !== null && !isString(taskId)) return null;
  if (attempt !== null && !isAttempt(attempt)) return null;
  const message: DialogMessage = { seq, ts, agent, task_id: taskId, attempt, text };
  // `code`/`params` (spec 102): auxiliares; fora do formato ficam ausentes.
  if (isMessageCode(value.code)) {
    message.code = value.code;
    const params = parseCodeParams(value.params);
    if (params !== null) message.params = params;
  }
  return message;
}

/** Resposta do diálogo: lista validada item a item (um item inválido invalida a página inteira). */
function toDialogView(value: unknown): DialogView | null {
  if (!isRecord(value)) return null;
  const messages = allOrNothing(value.messages, toDialogMessage);
  if (messages === null || !isSafeSeq(value.last_seq) || typeof value.truncated_before !== "boolean") return null;
  return { messages, last_seq: value.last_seq, truncated_before: value.truncated_before };
}

/**
 * GET /api/projects/:id/specs/:spec_id/dialog: diálogo persistido dos agentes, em ordem crescente
 * após `after`. Resposta fora do formato vira `invalid_response` (o chamador mostra a falha discreta).
 */
export function getDialog(projectId: string, specId: string, opts: DialogQuery = {}): Promise<ApiResult<DialogView>> {
  const params = new URLSearchParams();
  if (opts.after !== undefined) params.set("after", String(opts.after));
  if (opts.limit !== undefined) params.set("limit", String(opts.limit));
  const query = params.toString();
  return request<DialogView>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/dialog${query ? `?${query}` : ""}`,
  }).then((r) => {
    if (!r.ok) return r;
    const parsed = toDialogView(r.data);
    return parsed === null ? fail("invalid_response", 200, INVALID_RESPONSE_KEY) : { ok: true, data: parsed };
  });
}

/** Parâmetros de `getToolActions`: `task` filtra (`FINAL` = verificação final); `before` é o seq exclusivo. */
interface ToolActionsQuery {
  task?: string;
  before?: number;
  limit?: number;
}

/** `code`/`params` do instantâneo da atividade (spec 102): só os no formato fixo. */
/** `params` válidos da etapa do instantâneo, quando há. */
function stageParams(value: unknown): Pick<ActivitySnapshot, "stage_params"> {
  const params = parseCodeParams(value);
  return params === null || Object.keys(params).length === 0 ? {} : { stage_params: params };
}

function codedFields(it: Record<string, unknown>): Pick<ActivitySnapshot, "stage_code" | "tool_code" | "tool_params" | "text_code" | "text_params"> {
  const out: Pick<ActivitySnapshot, "stage_code" | "tool_code" | "tool_params" | "text_code" | "text_params"> = {};
  if (isMessageCode(it.stage_code)) out.stage_code = it.stage_code;
  const tool = parseCoded(it.tool_code, it.tool_params);
  if (tool !== undefined) {
    out.tool_code = tool.code;
    out.tool_params = tool.params;
  }
  const text = parseCoded(it.text_code, it.text_params);
  if (text !== undefined) {
    out.text_code = text.code;
    out.text_params = text.params;
  }
  return out;
}

/** Ação da trilha com tipos conferidos; `task_id` ausente vira `null`, `stage` só quando texto. */
function toToolAction(value: unknown): ToolActionItem | null {
  if (!isRecord(value)) return null;
  const { seq, ts, agent, kind, text, stage } = value;
  if (!isSafeSeq(seq) || typeof ts !== "number" || !Number.isFinite(ts)) return null;
  if (!isString(agent) || !isString(kind) || !isString(text)) return null;
  const taskId = value.task_id ?? null;
  if (taskId !== null && !isString(taskId)) return null;
  if (stage !== undefined && stage !== null && !isString(stage)) return null;
  const item: ToolActionItem = { seq, ts, agent, task_id: taskId, kind, text };
  if (isString(stage)) item.stage = stage;
  if (isMessageCode(value.stage_code)) item.stage_code = value.stage_code;
  const coded = parseCoded(value.code, value.params);
  if (coded !== undefined) {
    item.code = coded.code;
    item.params = coded.params;
  }
  return item;
}

function toToolActionsView(value: unknown): ToolActionsView | null {
  if (!isRecord(value)) return null;
  const items = allOrNothing(value.items, toToolAction);
  const { total, has_more } = value;
  if (items === null || typeof total !== "number" || !Number.isInteger(total) || total < 0 || typeof has_more !== "boolean") {
    return null;
  }
  return { items, total, has_more };
}

/**
 * GET /api/projects/:id/specs/:spec_id/tools: ações dos agentes (tools.jsonl) em ordem cronológica,
 * as `limit` mais recentes antes de `before`. Fora do formato vira `invalid_response`.
 */
export function getToolActions(projectId: string, specId: string, opts: ToolActionsQuery = {}): Promise<ApiResult<ToolActionsView>> {
  const params = new URLSearchParams();
  if (opts.task !== undefined) params.set("task", opts.task);
  if (opts.before !== undefined) params.set("before", String(opts.before));
  if (opts.limit !== undefined) params.set("limit", String(opts.limit));
  const query = params.toString();
  return request<ToolActionsView>({
    method: "GET",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/tools${query ? `?${query}` : ""}`,
  }).then((r) => {
    if (!r.ok) return r;
    const parsed = toToolActionsView(r.data);
    return parsed === null ? fail("invalid_response", 200, INVALID_RESPONSE_KEY) : { ok: true, data: parsed };
  });
}

/**
 * Falha HTTP de um turno do Arquiteto (`POST /chat` e `POST /chat/continue`).
 * 504 `code: "chat_timeout"`: o servidor cancelou o turno (chat.rs). Kind "timeout" com status 504.
 * 502 com causa fixa do provedor (chat.rs `architect_error`): mensagem específica + código; com
 * `reason: "connection_lost"` (spec 127 T01), o aviso da queda já está gravado no histórico.
 */
function chatHttpError(status: number, body: unknown): ApiFailure | null {
  if (status === 504 && serverErrorCode(body) === "chat_timeout") {
    const saved = serverReason(body) === "draft_saved";
    return { kind: "timeout", status: 504, ...clientReserve(TIMEOUT_KEY), ...(saved ? { reason: "draft_saved" } : {}) };
  }
  // 502 do chat é sempre falha do provedor: código desconhecido (ou legado) vira resposta
  // inesperada, nunca "falha interna do servidor".
  // `detail` (spec 088) só vale com código do vocabulário e passa por `providerFailureDetail`.
  // Spec 119: o motivo vem dos `params` neutros (`kind`, `provider_code`) e sai no idioma ativo;
  // um `detail` em pt-BR (servidor antigo) é lido de volta pelo dicionário.
  if (status === 502) {
    const providerCode = providerCodeOf(body);
    const reason = providerCode !== null ? providerReasonOf(body) : null;
    const legacy = providerCode !== null && reason === null && isRecord(body) ? providerFailureDetail(body.detail) : null;
    const providerDetail = reason?.text ?? (legacy === null ? null : localizeProviderDetail(legacy));
    return {
      kind: "http",
      status: 502,
      message:
        providerFailureMessage(providerCode ?? "provider_bad_response", null, providerDetail) ??
        SERVER_ERROR_MESSAGE,
      ...(providerCode === null ? {} : { providerCode }),
      ...(legacy === null ? {} : { providerDetail: legacy }),
      ...(reason === null ? {} : { providerKind: reason.kind }),
      ...(reason?.code === undefined ? {} : { providerKindCode: reason.code }),
      ...(serverReason(body) === "connection_lost" ? { reason: "connection_lost" } : {}),
    };
  }
  return null;
}

/**
 * POST /api/projects/:id/chat com `{message}`. `internal: true` marca mensagem do sistema
 * (ex.: pedido automático ao Arquiteto): o servidor a omite do histórico exibido. `attachments`
 * (até 4 nomes devolvidos pelo upload) só vai no corpo quando não vazio.
 */
export function postChat(
  projectId: string,
  message: string,
  options: { internal?: boolean; signal?: AbortSignal; attachments?: readonly string[] } = {},
): Promise<ApiResult<ChatReply>> {
  const body: { message: string; internal?: boolean; attachments?: readonly string[] } =
    options.internal === true ? { message, internal: true } : { message };
  if (options.attachments !== undefined && options.attachments.length > 0) body.attachments = options.attachments;
  return request<ChatReply>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/chat`,
    body: JSON.stringify(body),
    timeoutMs: CHAT_TIMEOUT_MS,
    ...(options.signal === undefined ? {} : { signal: options.signal }),
    onHttpError: chatHttpError,
  }).then((r) => asObject<ChatReply>(r));
}

/**
 * POST /api/projects/:id/chat/continue (spec 127 T01): turno interno "continue de onde parou",
 * sem texto do usuário. Com `specId`, pede ao Arquiteto para terminar o plano sem tarefas dessa
 * spec. Mesma resposta e mesmas falhas do `postChat`.
 */
export function postChatContinue(projectId: string, specId: string | null = null): Promise<ApiResult<ChatReply>> {
  return request<ChatReply>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/chat/continue`,
    body: JSON.stringify(specId === null ? {} : { spec_id: specId }),
    timeoutMs: CHAT_TIMEOUT_MS,
    onHttpError: chatHttpError,
  }).then((r) => asObject<ChatReply>(r));
}

/**
 * Motivo neutro (`params.kind` e `params.provider_code`) do `provider_failed` (spec 119), com o
 * rótulo já no idioma ativo; `null` sem `kind` do vocabulário.
 */
function providerReasonOf(body: unknown): { kind: string; code?: string; text: string } | null {
  if (!isRecord(body) || !isRecord(body.params)) return null;
  const { kind, provider_code: code } = body.params;
  const text = providerReasonText(kind, code);
  if (text === null || typeof kind !== "string") return null;
  return typeof code === "string" && text.endsWith(`: ${code}`) ? { kind, code, text } : { kind, text };
}

/** Causa fixa do provedor (`params.provider` do `provider_failed`); nunca devolve o texto cru. */
function providerCodeOf(body: unknown): string | null {
  if (!isRecord(body) || !isRecord(body.params)) return null;
  const provider = body.params.provider;
  return typeof provider === "string" && isProviderFailureCode(provider) ? provider : null;
}

/** POST /api/projects/:id/specs/:spec_id/approve (202 em sucesso). */
export function approveSpec(projectId: string, specId: string): Promise<ApiResult<ApproveResult>> {
  return request<ApproveResult>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/approve`,
    body: "{}",
  }).then((r) => asObject<ApproveResult>(r));
}

/**
 * POST /api/projects/:id/specs/:spec_id/resume (202 com o corpo do approve em sucesso).
 * Mesmos tratamentos de erro de `approveSpec`: 409/422 com códigos fixos, 5xx e rede estáticos.
 */
export function resumeSpec(projectId: string, specId: string): Promise<ApiResult<ApproveResult>> {
  return request<ApproveResult>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/resume`,
    body: "{}",
  }).then((r) => asObject<ApproveResult>(r));
}

/** Resposta 202 de `stopSpec`. */
interface StopResult {
  stopping: boolean;
}

/**
 * POST /api/projects/:id/specs/:spec_id/stop (spec 086 T04): pede a parada da execução ativa da spec.
 * 202 `{stopping: true}` (idempotente); 409 sem execução ativa daquela spec; 404/422 como as rotas irmãs.
 */
export function stopSpec(projectId: string, specId: string): Promise<ApiResult<StopResult>> {
  return request<StopResult>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/stop`,
    body: "{}",
  }).then((r) => asObject<StopResult>(r));
}

/**
 * POST /api/projects/:id/specs/:spec_id/tasks/:task_id/recovery-choice (202).
 * 409 (`reason` opcional), 422 e 404 seguem como falha com status e reason.
 */
export function chooseRecovery(
  projectId: string,
  specId: string,
  taskId: string,
  body: RecoveryChoiceBody,
): Promise<ApiResult<RecoveryChoiceResult>> {
  return request<RecoveryChoiceResult>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/specs/${enc(specId)}/tasks/${enc(taskId)}/recovery-choice`,
    body: JSON.stringify(body),
  }).then((r) => asObject<RecoveryChoiceResult>(r));
}

/** Resultado de `removeProject`. `alreadyRemoved` é `true` quando o servidor já não tinha o projeto (404). */
interface RemoveProjectResult {
  removed: string;
  alreadyRemoved: boolean;
}


/**
 * DELETE /api/projects/:id: tira o projeto só do Nants (arquivos no disco permanecem).
 * Corpo `{}` (ignorado pelo servidor). 404 é sucesso idempotente: o projeto já não existe.
 * 409 (execução ou turno de chat em andamento) e demais erros seguem como falha com mensagem fixa.
 */
export function removeProject(
  projectId: string,
  opts?: { removeVolumes?: boolean },
): Promise<ApiResult<RemoveProjectResult>> {
  return request<{ ok?: unknown; removed?: unknown }>({
    method: "DELETE",
    path: `/api/projects/${enc(projectId)}`,
    body: opts?.removeVolumes === true ? JSON.stringify({ remove_volumes: true }) : "{}",
  }).then((r): ApiResult<RemoveProjectResult> => {
    if (!r.ok) {
      if (r.error.status === 404 && r.error.code === "project_not_found") {
        return { ok: true, data: { removed: projectId, alreadyRemoved: true } };
      }
      return r;
    }
    const removed = isRecord(r.data) ? r.data.removed : undefined;
    if (typeof removed !== "string") return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
    return { ok: true, data: { removed, alreadyRemoved: false } };
  });
}

/** POST /api/projects/:id/permissions/:req_id com `{decision}`. */
export function resolvePermission(
  projectId: string,
  requestId: string,
  decision: PermissionDecision,
): Promise<ApiResult<ResolveResult>> {
  return request<ResolveResult>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/permissions/${enc(requestId)}`,
    body: JSON.stringify({ decision }),
  }).then((r) => asObject<ResolveResult>(r));
}

/** GET /api/fs/browse (path opcional; hidden mostra ocultos). */
export function browse(opts?: { path?: string; hidden?: boolean }): Promise<ApiResult<BrowseView>> {
  const params = new URLSearchParams();
  if (opts?.path !== undefined) params.set("path", opts.path);
  if (opts?.hidden) params.set("hidden", "true");
  const query = params.toString();
  return request<BrowseView>({
    method: "GET",
    path: `/api/fs/browse${query ? `?${query}` : ""}`,
  }).then((r) => asObject<BrowseView>(r));
}

// ---------- App de teste (spec 021) ----------

type AppRunState = "stopped" | "running" | "exited";

/** Resposta validada de `GET .../app`. */
export interface AppStatus {
  state: AppRunState;
  spec_id: string | null;
  url: string | null;
  started_at: string | null;
  exit_code: number | null;
  /** Portas publicadas pelo Compose `-run` (modo docker); vazio no modo host. */
  ports: AppPort[];
  /**
   * Avisos curtos do servidor (ex.: porta do host trocada): o texto pt-BR de reserva e, quando o
   * servidor manda (`notice_codes`, spec 102), o `code` e os `params` para traduzir.
   */
  notices: AppNotice[];
  /**
   * A página do app pode ser embutida (spec 093 T02, decisão 12): o da primeira porta com link.
   * `null` = desconhecido (sonda sem resposta ou sem portas).
   */
  embeddable: boolean | null;
  /**
   * Porta do Nants, faixa de visualização e WSL (spec 097, T01), no nível raiz da resposta; `null`
   * em servidores antigos.
   */
  network: NetworkView | null;
}

/** Aviso do app: texto de reserva (pt-BR) e, opcionalmente, `code`/`params` (spec 102). */
export interface AppNotice {
  text: string;
  code?: string;
  params?: CodeParams;
}

/** `notices` com os `notice_codes` do mesmo índice (formato fixo; o resto é ignorado). */
function toAppNotices(notices: unknown, codes: unknown): AppNotice[] {
  if (!Array.isArray(notices)) return [];
  const texts = notices.filter((n): n is string => isString(n)).slice(0, 8).map((n) => n.slice(0, 200));
  return texts.map((text, index) => {
    const entry: unknown = Array.isArray(codes) ? codes[index] : undefined;
    if (!isRecord(entry) || !isMessageCode(entry.code)) return { text };
    const params = parseCodeParams(entry.params);
    return params === null ? { text, code: entry.code } : { text, code: entry.code, params };
  });
}

/** Porta publicada pelo ambiente Docker, com o link já montado pelo servidor com o host da requisição. */
export interface AppPort {
  service: string;
  target: number;
  published: number;
  url: string;
  /** Veredito da sonda `embeddable` desta porta (`null` = desconhecido). */
  embeddable: boolean | null;
}

const APP_STATES: ReadonlySet<string> = new Set(["stopped", "running", "exited"]);
/** Códigos `reason` de erro do app de teste que a UI conhece; outros ficam sem `reason`. */
const APP_ERROR_REASONS: ReadonlySet<string> = new Set([
  "spec_not_completed",
  "busy",
  "already_running",
  "port_in_use",
  "no_run_command",
  "docker_failed",
  "docker_port_in_use",
  "run_command_docker",
  "environment_not_docker",
]);

/** Limite do detalhe técnico mostrado na UI. */
const MAX_APP_DETAIL = 4000;

function appReasonOnly(status: number, body: unknown): ApiFailure | null {
  // 412 `prereq_missing` (spec 093 T02): segue o caminho comum, que guarda os ids do que falta.
  if (status === 412) return null;
  const base: ApiFailure = { kind: "http", status, ...clientReserve(statusKey(status)) };
  if (isRecord(body) && typeof body.reason === "string" && APP_ERROR_REASONS.has(body.reason)) {
    base.reason = body.reason;
    if ((body.reason === "docker_failed" || body.reason === "docker_port_in_use") && typeof body.detail === "string" && body.detail.trim() !== "") {
      base.detail = body.detail.slice(-MAX_APP_DETAIL);
    }
  }
  return base;
}

function toAppStatus(value: unknown): AppStatus | null {
  if (!isRecord(value) || !isString(value.state) || !APP_STATES.has(value.state)) return null;
  const str = (v: unknown): string | null => (isString(v) ? v : null);
  return {
    state: value.state as AppRunState,
    spec_id: str(value.spec_id),
    url: str(value.url),
    started_at: str(value.started_at),
    exit_code: typeof value.exit_code === "number" && Number.isSafeInteger(value.exit_code) ? value.exit_code : null,
    ports: toAppPorts(value.ports),
    notices: toAppNotices(value.notices, value.notice_codes),
    embeddable: toEmbeddable(value.embeddable),
    network: parseNetworkView(value),
  };
}

function toEmbeddable(value: unknown): boolean | null {
  return value === true || value === false ? value : null;
}

function toAppPorts(value: unknown): AppPort[] {
  if (!Array.isArray(value)) return [];
  const out: AppPort[] = [];
  for (const item of value.slice(0, 32)) {
    if (!isRecord(item) || !isString(item.service) || !isString(item.url)) continue;
    const { target, published } = item;
    if (typeof target !== "number" || typeof published !== "number") continue;
    const url = portLinkUrl(item.url, published);
    if (url === null) continue;
    out.push({ service: item.service.slice(0, 64), target, published, url, embeddable: toEmbeddable(item.embeddable) });
  }
  return out;
}

/**
 * Link de porta publicada: `http://<host>:<porta>` onde o host é o MESMO da página (assim o link
 * funciona no celular, pela LAN) e a porta é a publicada. Qualquer outra forma não vira link.
 */
function portLinkUrl(value: string, published: number): string | null {
  if (value.length > 200 || !Number.isInteger(published) || published < 1 || published > 65535) return null;
  const m = /^http:\/\/(\[[0-9a-fA-F:]+\]|[A-Za-z0-9.-]+):(\d{1,5})$/.exec(value);
  if (m === null || Number(m[2]) !== published) return null;
  return m[1] === window.location.hostname ? value : null;
}

/** GET /api/projects/:id/app. */
export function getAppStatus(projectId: string): Promise<ApiResult<AppStatus>> {
  return request<unknown>({ method: "GET", path: `/api/projects/${enc(projectId)}/app` }).then((r) => {
    if (!r.ok) return r;
    const parsed = toAppStatus(r.data);
    return parsed === null ? fail("invalid_response", 200, INVALID_RESPONSE_KEY) : { ok: true, data: parsed };
  });
}

/** POST /api/projects/:id/app/start com `{confirm: true, spec_id}`. Só chamar após a confirmação humana. */
export function startApp(projectId: string, specId: string): Promise<ApiResult<AppStatus | null>> {
  return request<unknown>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/app/start`,
    body: JSON.stringify({ confirm: true, spec_id: specId }),
    timeoutMs: APP_LIFECYCLE_TIMEOUT_MS,
    onHttpError: appReasonOnly,
  }).then((r) => (r.ok ? { ok: true, data: toAppStatus(r.data) } : r));
}

/** POST /api/projects/:id/app/stop. */
export function stopApp(projectId: string): Promise<ApiResult<null>> {
  return request<unknown>({
    method: "POST",
    path: `/api/projects/${enc(projectId)}/app/stop`,
    body: "{}",
    timeoutMs: APP_LIFECYCLE_TIMEOUT_MS,
    onHttpError: appReasonOnly,
  }).then((r) => (r.ok ? { ok: true, data: null } : r));
}

/** GET /api/projects/:id/app/logs: texto puro (redigido pelo servidor, até 64 KiB). */
export async function getAppLogs(projectId: string): Promise<ApiResult<string>> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), DEFAULT_TIMEOUT_MS);
  try {
    const response = await fetch(`/api/projects/${enc(projectId)}/app/logs`, {
      method: "GET",
      credentials: "same-origin",
      signal: controller.signal,
    });
    if (!response.ok) return fail("http", response.status, statusKey(response.status));
    return { ok: true, data: await response.text() };
  } catch {
    return fail("network", 0, NETWORK_KEY);
  } finally {
    clearTimeout(timer);
  }
}

// ---------- Explorador de arquivos (spec 066, somente leitura) ----------

/** Fonte da árvore: a base integrada ou a worktree da tarefa ativa (seletor na T04). */
export type FileSource = "base" | "task";

type FileEntryKind = "file" | "symlink" | "ignored_dir" | "dir";

export interface FileEntry {
  /** Caminho completo a partir da raiz. */
  path: string;
  kind: FileEntryKind;
}

interface FileListing {
  source: FileSource;
  dir?: string;
  entries: FileEntry[];
  truncated: boolean;
  /** Só em `source=task` (raiz): tarefa ativa e arquivos alterados na tarefa. */
  taskId?: string;
  changed?: string[];
}

export type FileContent =
  | { kind: "text"; size: number; content: string; masked: boolean }
  | { kind: "binary"; size: number }
  | { kind: "too_large"; size: number }
  /** `diff=1` em `source=task`: diff unificado do arquivo contra a base. */
  | { kind: "diff"; size: number; content: string };

/** Resultado da leitura: conteúdo novo (com ETag) ou 304 (nada mudou desde o ETag enviado). */
type FileRead =
  | { status: "ok"; file: FileContent; etag: string | null }
  | { status: "not_modified" };

const FILE_ENTRY_KINDS: ReadonlySet<string> = new Set(["file", "symlink", "ignored_dir", "dir"]);

function filesBase(projectId: string): string {
  return `/api/projects/${enc(projectId)}`;
}

/** GET /api/projects/:id/files?source=&dir= (sem `dir`: lista plana da raiz). */
export function getProjectFiles(
  projectId: string,
  source: FileSource,
  dir?: string,
): Promise<ApiResult<FileListing>> {
  const query = `source=${enc(source)}${dir === undefined ? "" : `&dir=${enc(dir)}`}`;
  return request<unknown>({ method: "GET", path: `${filesBase(projectId)}/files?${query}` }).then((r) => {
    const result = asObject<{ entries?: unknown; truncated?: unknown; task_id?: unknown; changed?: unknown }>(r);
    if (!result.ok) return result;
    const raw = result.data.entries;
    if (!Array.isArray(raw)) return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
    const entries: FileEntry[] = [];
    for (const item of raw) {
      if (!isRecord(item) || typeof item.path !== "string" || typeof item.kind !== "string") {
        return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
      }
      if (!FILE_ENTRY_KINDS.has(item.kind)) continue; // tipo desconhecido: ignora
      entries.push({ path: item.path, kind: item.kind as FileEntryKind });
    }
    const changed = Array.isArray(result.data.changed)
      ? result.data.changed.filter((p): p is string => typeof p === "string")
      : undefined;
    const taskId = typeof result.data.task_id === "string" ? result.data.task_id : undefined;
    return {
      ok: true,
      data: {
        source,
        ...(dir === undefined ? {} : { dir }),
        entries,
        truncated: result.data.truncated === true,
        ...(taskId === undefined ? {} : { taskId }),
        ...(changed === undefined ? {} : { changed }),
      },
    };
  });
}

/** GET /api/projects/:id/file?source=&path=[&reveal=1], com `If-None-Match` opcional. */
export function getProjectFile(
  projectId: string,
  source: FileSource,
  path: string,
  opts: { reveal?: boolean; diff?: boolean; etag?: string | null } = {},
): Promise<ApiResult<FileRead>> {
  const query = `source=${enc(source)}&path=${enc(path)}${opts.reveal === true ? "&reveal=1" : ""}${opts.diff === true ? "&diff=1" : ""}`;
  let etag: string | null = null;
  const headers: Record<string, string> = {};
  if (typeof opts.etag === "string" && opts.etag !== "") headers["If-None-Match"] = opts.etag;
  return request<unknown>({
    method: "GET",
    path: `${filesBase(projectId)}/file?${query}`,
    headers,
    onResponse: (response) => {
      etag = response.headers.get("ETag");
    },
    onHttpError: (status) => (status === 304 ? { kind: "http", status: 304, message: "não modificado" } : null),
  }).then((r): ApiResult<FileRead> => {
    if (!r.ok) {
      return r.error.status === 304 ? { ok: true, data: { status: "not_modified" } } : r;
    }
    const body = r.data;
    if (!isRecord(body) || typeof body.size !== "number") return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
    if (body.kind === "text" && typeof body.content === "string") {
      return {
        ok: true,
        data: {
          status: "ok",
          etag,
          file: { kind: "text", size: body.size, content: body.content, masked: body.masked === true },
        },
      };
    }
    if (body.kind === "diff" && typeof body.content === "string") {
      return { ok: true, data: { status: "ok", etag, file: { kind: "diff", size: body.size, content: body.content } } };
    }
    if (body.kind === "binary" || body.kind === "too_large") {
      return { ok: true, data: { status: "ok", etag, file: { kind: body.kind, size: body.size } } };
    }
    return fail("invalid_response", 200, INVALID_RESPONSE_KEY);
  });
}
