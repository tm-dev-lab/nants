/*
 * A conexão de cada papel (spec 133 T02 e T03): regras puras, sem React.
 *
 * - Quais conexões estão prontas para receber um papel (o plano com login, as outras com chave).
 * - O nome do tipo da conexão (marca), para dizer "Anthropic", "Google Gemini", "Plano ChatGPT".
 * - A página oficial de limites ou de cobrança do provedor, pelo tipo da conexão (e, na conexão por
 *   chave, pelo endereço oficial da OpenAI). Sem página conhecida, nenhum link.
 * - A troca da conexão de um papel: a configuração dos agentes com só aquele papel mudado.
 */
import type { AgentConfig, Agents, ConnectionView } from "../../api/types";
import { t, type MessageKey } from "../../i18n";
import { connectionKindOf, type AgentRole } from "../../settings/rules";
import { PLAN_USAGE_URL } from "../settings/chatgptPlan";

/** A conexão pode receber um papel agora? O plano precisa de login; as outras, de uma chave salva. */
function isReadyConnection(connection: ConnectionView): boolean {
  if (connectionKindOf(connection) === "chatgpt_plan") return connection.chatgpt?.state === "connected";
  return connection.api_key_masked.trim() !== "";
}

/** Conexões prontas, na ordem salva. */
export function readyConnections(connections: readonly ConnectionView[]): ConnectionView[] {
  return connections.filter(isReadyConnection);
}

const KIND_KEY: Readonly<Record<ReturnType<typeof connectionKindOf>, MessageKey>> = {
  chatgpt_plan: "roleConn.kind.chatgpt_plan",
  anthropic: "roleConn.kind.anthropic",
  google_gemini: "roleConn.kind.google_gemini",
  api_key: "roleConn.kind.api_key",
};

/** Nome do tipo da conexão, no idioma ativo (as marcas são iguais nos dois). */
export function connectionKindLabel(kind: unknown): string {
  return t(KIND_KEY[connectionKindOf({ kind })]);
}

/** O que parou o papel: decide se o link leva à cobrança ou aos limites. */
type LimitTopic = "billing" | "limits";

/** Endereço oficial da OpenAI: a conexão por chave que aponta para ele ganha as páginas da OpenAI. */
const OPENAI_API_HOST = "api.openai.com";

const PAGES: Readonly<Record<"anthropic" | "google_gemini" | "openai", Record<LimitTopic, string>>> = {
  anthropic: {
    billing: "https://console.anthropic.com/settings/billing",
    limits: "https://console.anthropic.com/settings/limits",
  },
  google_gemini: {
    billing: "https://aistudio.google.com/usage",
    limits: "https://aistudio.google.com/usage",
  },
  openai: {
    billing: "https://platform.openai.com/settings/organization/billing/overview",
    limits: "https://platform.openai.com/settings/organization/limits",
  },
};

interface ProviderPage {
  url: string;
  label: string;
}

function hostOf(url: string): string | null {
  try {
    return new URL(url).hostname;
  } catch {
    return null;
  }
}

/**
 * Página oficial de limites ou de cobrança da conexão, ou `null` quando o Nants não conhece uma
 * (uma conexão compatível qualquer). O plano ChatGPT tem uma só, a de uso.
 */
export function providerPage(connection: Pick<ConnectionView, "kind" | "base_url"> | null, kindHint: unknown, topic: LimitTopic): ProviderPage | null {
  const kind = connectionKindOf({ kind: connection?.kind ?? kindHint });
  if (kind === "chatgpt_plan") return { url: PLAN_USAGE_URL, label: t("chatgpt.manageUsage") };
  const label = topic === "billing" ? t("roleConn.page.billing") : t("roleConn.page.limits");
  if (kind === "anthropic" || kind === "google_gemini") return { url: PAGES[kind][topic], label };
  if (connection !== null && hostOf(connection.base_url) === OPENAI_API_HOST) return { url: PAGES.openai[topic], label };
  return null;
}

/** Configuração dos agentes com só `role` trocado para a conexão e o modelo dados. */
export function withRoleConnection(agents: Agents, role: AgentRole, connectionId: string, model: string): Agents {
  const next: AgentConfig = { ...agents[role], connection_id: connectionId, model: model.trim() };
  return { ...agents, [role]: next };
}

/** O papel nos `params` do servidor, quando é um dos três. */
export function roleOf(value: unknown): AgentRole | null {
  return value === "architect" || value === "developer" || value === "reviewer" ? value : null;
}

/**
 * Modelo sugerido ao escolher outra conexão para o papel: o que outro papel já usa nela (a pessoa
 * já escolheu esse modelo para essa conta); senão, vazio, e a lista da conexão completa.
 */
export function suggestedModel(agents: Agents, connectionId: string): string {
  for (const role of ["architect", "developer", "reviewer"] as const) {
    if (agents[role].connection_id === connectionId && agents[role].model.trim() !== "") return agents[role].model;
  }
  return "";
}
