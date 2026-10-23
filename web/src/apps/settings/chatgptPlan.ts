/*
 * Conexão pelo plano ChatGPT na interface (spec 116 T03): regras puras e o retorno do login.
 *
 * - O login acontece na mesma janela: o botão "Continue with ChatGPT" pede a URL de autorização ao
 *   servidor e navega até ela (abrir aba depois de um `await` cai no bloqueio de pop-ups). Antes de
 *   sair, guarda o espaço aberto na `sessionStorage`, para voltar a ele.
 * - O servidor devolve a pessoa para `/?chatgpt=connected&connection=<id>` ou
 *   `/?chatgpt=error&reason=<motivo>`. `captureChatgptReturn` lê esses parâmetros na carga, antes
 *   do primeiro render (a rota do desktop normaliza a URL e os apagaria), e devolve o endereço do
 *   espaço guardado.
 * - O resultado fica aqui (`useChatgptReturn`): o shell abre Configurações > Conexões, as boas-vindas
 *   aparecem uma vez só (seção global `chatgpt` do ui-state) e a aba mostra a conexão pronta ou o
 *   erro traduzido.
 */
import { useSyncExternalStore } from "react";
import type { Agents, ChatgptPlanStatus, ConnectionView } from "../../api/types";
import { t, type MessageKey } from "../../i18n";

/** Onde a pessoa acompanha e ajusta o uso do plano (link "Gerenciar uso"). */
export const PLAN_USAGE_URL = "https://chatgpt.com/settings/usage";

/** Chave da `sessionStorage` com o espaço aberto quando o login começou. */
export const RETURN_SPACE_KEY = "nants.chatgpt.returnSpace";

/** A conexão é pelo plano ChatGPT? */
export function isPlanConnection(connection: Pick<ConnectionView, "kind">): boolean {
  return connection.kind === "chatgpt_plan";
}

/** Estado do login da conexão pelo plano (sem estado vindo do servidor: desconectada). */
export function planState(connection: ConnectionView): ChatgptPlanStatus["state"] {
  return connection.chatgpt?.state ?? "disconnected";
}

/** Estado do login em texto curto, no idioma ativo ("conectada", "precisa entrar de novo"...). */
export function planStateText(state: ReturnType<typeof planState>): string {
  if (state === "connected") return t("chatgpt.state.connected");
  return state === "disconnected" ? t("chatgpt.state.disconnected") : t("chatgpt.state.needsLogin");
}

/** E-mail da conta da conexão pelo plano (vazio sem login). */
export function planEmail(connection: ConnectionView): string {
  const status = connection.chatgpt;
  return status !== undefined && status.state !== "disconnected" ? status.email : "";
}

/** Algum papel usa uma conexão pelo plano ChatGPT? */
export function agentsUsePlan(agents: Agents | null | undefined, connections: readonly ConnectionView[] | undefined): boolean {
  if (agents == null || connections === undefined) return false;
  const plan = new Set(connections.filter(isPlanConnection).map((c) => c.id));
  return plan.size > 0 && [agents.architect, agents.developer, agents.reviewer].some((a) => plan.has(a.connection_id));
}

/**
 * Conexão pelo plano a reconectar quando a execução pausou pedindo login: a primeira usada por um
 * papel que precisa entrar de novo; senão, a primeira usada por um papel.
 */
export function planConnectionToReconnect(agents: Agents | null | undefined, connections: readonly ConnectionView[] | undefined): ConnectionView | null {
  if (agents == null || connections === undefined) return null;
  const used = new Set([agents.architect, agents.developer, agents.reviewer].map((a) => a.connection_id));
  const plans = connections.filter((c) => isPlanConnection(c) && used.has(c.id));
  return plans.find((c) => planState(c) !== "connected") ?? plans[0] ?? null;
}

/** Motivos de recusa do login que o servidor manda no retorno, cada um com o texto. */
const LOGIN_ERROR_KEYS: Readonly<Record<string, MessageKey>> = {
  state_invalid: "chatgpt.loginError.state_invalid",
  access_denied: "chatgpt.loginError.access_denied",
  authorize_failed: "chatgpt.loginError.authorize_failed",
  code_missing: "chatgpt.loginError.code_missing",
  client_id_missing: "chatgpt.loginError.client_id_missing",
  client_id_mismatch: "chatgpt.loginError.client_id_mismatch",
  token_exchange_failed: "chatgpt.loginError.token_exchange_failed",
  token_response_invalid: "chatgpt.loginError.token_response_invalid",
  id_token_invalid: "chatgpt.loginError.id_token_invalid",
  scope_missing: "chatgpt.loginError.scope_missing",
  store_failed: "chatgpt.loginError.store_failed",
};

/** Motivos conhecidos (os testes conferem a lista contra o servidor). */
export const LOGIN_ERROR_REASONS: readonly string[] = Object.keys(LOGIN_ERROR_KEYS);

/** Texto do motivo da recusa do login, no idioma ativo (motivo desconhecido: texto geral). */
export function loginErrorText(reason: string): string {
  const key = Object.hasOwn(LOGIN_ERROR_KEYS, reason) ? LOGIN_ERROR_KEYS[reason] : undefined;
  return t(key ?? "chatgpt.loginError.unknown");
}

// --- Retorno do login ---

type ChatgptReturn = { kind: "connected"; connectionId: string } | { kind: "error"; reason: string };

/** Mesmo formato de id de conexão aceito pelo servidor. */
const CONNECTION_ID = /^[A-Za-z0-9_-]{1,64}$/;
const REASON = /^[a-z_]{1,64}$/;
const SPACE_ID = /^[A-Za-z0-9_-]{1,64}$/;

/** Lê o resultado do login nos parâmetros da URL (`null` sem `chatgpt` ou com valor inválido). */
export function parseChatgptReturn(search: string): ChatgptReturn | null {
  const params = new URLSearchParams(search);
  const kind = params.get("chatgpt");
  if (kind === "connected") {
    const id = params.get("connection") ?? "";
    return CONNECTION_ID.test(id) ? { kind: "connected", connectionId: id } : null;
  }
  if (kind === "error") {
    const reason = params.get("reason") ?? "";
    return { kind: "error", reason: REASON.test(reason) ? reason : "unknown" };
  }
  return null;
}

interface ReturnState {
  result: ChatgptReturn | null;
  /** As boas-vindas ainda podem aparecer (só depois de um login concluído nesta carga). */
  welcome: boolean;
}

let state: ReturnState = { result: null, welcome: false };
const listeners = new Set<() => void>();

function publish(next: ReturnState): void {
  state = next;
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function storage(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

/** Guarda o espaço aberto antes de sair para o login (para voltar a ele). */
export function rememberReturnSpace(spaceId: string | null): void {
  const store = storage();
  if (store === null) return;
  try {
    if (spaceId === null) store.removeItem(RETURN_SPACE_KEY);
    else store.setItem(RETURN_SPACE_KEY, spaceId);
  } catch {
    // Armazenamento cheio ou bloqueado: a volta cai nas boas-vindas.
  }
}

/**
 * Na carga da página: guarda o resultado do login que veio na URL e devolve o endereço a usar
 * (o espaço guardado, ou `/`), ou `null` quando a URL não traz retorno de login.
 */
export function captureChatgptReturn(search: string): string | null {
  const result = parseChatgptReturn(search);
  if (result === null && !new URLSearchParams(search).has("chatgpt")) return null;
  const store = storage();
  let space: string | null = null;
  try {
    space = store?.getItem(RETURN_SPACE_KEY) ?? null;
    store?.removeItem(RETURN_SPACE_KEY);
  } catch {
    space = null;
  }
  const resolved = result ?? { kind: "error" as const, reason: "unknown" };
  publish({ result: resolved, welcome: resolved.kind === "connected" });
  return space !== null && SPACE_ID.test(space) ? `/?space=${encodeURIComponent(space)}` : "/";
}

/** Resultado do login ainda não tratado (reativo). */
export function useChatgptReturn(): ReturnState {
  return useSyncExternalStore(subscribe, () => state, () => state);
}

/** O resultado já foi mostrado (a aba de Conexões o dispensa). */
export function dismissChatgptReturn(): void {
  if (state.result !== null) publish({ ...state, result: null });
}

/** As boas-vindas foram vistas (ou não cabem mais nesta carga). */
export function dismissChatgptWelcome(): void {
  if (state.welcome) publish({ ...state, welcome: false });
}

/** Testes: volta ao estado inicial. */
export function resetChatgptReturn(): void {
  publish({ result: null, welcome: false });
}

/** Seção global do ui-state: `{welcomed: true}` depois das boas-vindas (uma vez só). */
export function parseChatgptSection(value: unknown): { welcomed: boolean } {
  return { welcomed: typeof value === "object" && value !== null && (value as { welcomed?: unknown }).welcomed === true };
}
