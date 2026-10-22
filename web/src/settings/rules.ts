/*
 * Regras puras de Conexões & Agentes (sem React, testáveis).
 * Espelham as validações do servidor (config_api.rs) para feedback imediato; o servidor
 * continua sendo a autoridade. Mensagens são fixas e saem do dicionário no idioma ativo (spec 102
 * T05): as tabelas usam `localized` ou getters (cada leitura devolve o texto do idioma atual).
 */
import type { ConfigView, Effort, TestConnectionResult, WireApi } from "../api/types";
import { localized, t, tp } from "../i18n";

const ID_MAX_LENGTH = 64;
const TEXT_MAX_LENGTH = 128;

/** Níveis de esforço: os nomes técnicos (Low, Med, High) são iguais nos dois idiomas. */
export const EFFORT_OPTIONS: readonly { value: Effort; label: string }[] = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Med" },
  { value: "high", label: "High" },
];

export type AgentRole = "architect" | "developer" | "reviewer";

/** Papel com o nome no idioma ativo (getter) e o esforço sugerido. */
export const ROLE_INFO: Readonly<Record<AgentRole, { readonly label: string; defaultEffort: Effort }>> = {
  architect: {
    get label() {
      return t("role.architect");
    },
    defaultEffort: "low",
  },
  developer: {
    get label() {
      return t("role.developer");
    },
    defaultEffort: "low",
  },
  reviewer: {
    get label() {
      return t("role.reviewer");
    },
    defaultEffort: "medium",
  },
};

export const ROLE_ORDER: readonly AgentRole[] = ["architect", "developer", "reviewer"];

/**
 * Limite de passos por agente (spec 081): rede de segurança, o orçamento controla o custo.
 * Só Desenvolvedor e Revisor; vazio usa o padrão do servidor. Faixa igual à de config_api.rs.
 */
export const MAX_STEPS_MIN = 10;
export const MAX_STEPS_MAX = 1000;
export type StepsRole = "developer" | "reviewer";
export const DEFAULT_MAX_STEPS: Readonly<Record<StepsRole, number>> = { developer: 150, reviewer: 40 };

export function hasStepsLimit(role: AgentRole): role is StepsRole {
  return role === "developer" || role === "reviewer";
}

/** Texto do campo → valor: vazio = padrão (`undefined`); fora da faixa ou não inteiro = erro. */
export function parseMaxSteps(text: string): { ok: true; value: number | undefined } | { ok: false; message: string } {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: true, value: undefined };
  if (!/^\d+$/.test(trimmed)) return { ok: false, message: MSG.maxStepsRange };
  const value = Number(trimmed);
  if (value < MAX_STEPS_MIN || value > MAX_STEPS_MAX) return { ok: false, message: MSG.maxStepsRange };
  return { ok: true, value };
}

const ID_RE = /^[a-zA-Z0-9_-]+$/;
const ENV_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]{0,63}$/;

/** Mensagens fixas de validação (mesma ordem de campos do formulário de conexão), no idioma ativo. */
export const MSG = localized({
  idRequired: "settings.validation.idRequired",
  idInvalid: "settings.validation.idInvalid",
  nameRequired: "settings.validation.nameRequired",
  nameTooLong: "settings.validation.nameTooLong",
  urlRequired: "settings.validation.urlRequired",
  urlInvalid: "settings.validation.urlInvalid",
  urlUserinfo: "settings.validation.urlUserinfo",
  urlQuery: "settings.validation.urlQuery",
  keyRequired: "settings.validation.keyRequired",
  keyEnvInvalid: "settings.validation.keyEnvInvalid",
  modelRequired: "settings.validation.modelRequired",
  modelTooLong: "settings.validation.modelTooLong",
  connectionRequired: "settings.validation.connectionRequired",
  maxStepsRange: "settings.validation.maxStepsRange",
});

export type FieldErrors = Partial<Record<"id" | "name" | "base_url" | "api_key", string>>;

/** Conta code points (como `chars().count()` no servidor), não unidades UTF-16. */
function codePoints(value: string): number {
  return Array.from(value).length;
}

/** Espelha `char::is_control`: C0, DEL e C1 (U+0080–U+009F). */
function hasControl(value: string): boolean {
  return /[\u0000-\u001f\u007f-\u009f]/.test(value);
}

export function validateConnectionId(value: string): string | null {
  if (value.length === 0) return MSG.idRequired;
  if (codePoints(value) > ID_MAX_LENGTH || !ID_RE.test(value)) return MSG.idInvalid;
  return null;
}

export function validateName(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return MSG.nameRequired;
  if (codePoints(trimmed) > TEXT_MAX_LENGTH || hasControl(trimmed)) return MSG.nameTooLong;
  return null;
}

/** http(s) com host, sem userinfo, query ou fragmento. */
export function validateBaseUrl(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return MSG.urlRequired;
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return MSG.urlInvalid;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return MSG.urlInvalid;
  if (url.hostname === "") return MSG.urlInvalid;
  if (url.username !== "" || url.password !== "") return MSG.urlUserinfo;
  if (url.search !== "" || url.hash !== "" || trimmed.includes("?") || trimmed.includes("#")) {
    return MSG.urlQuery;
  }
  return null;
}

/**
 * Forma normalizada de `base_url` para comparar endpoints (mesma regra do servidor: esquema e host
 * em minúsculas, sem barra final). `null` se a URL não for válida.
 */
export function normalizeBaseUrl(value: string): string | null {
  try {
    const url = new URL(value.trim());
    if (url.protocol !== "http:" && url.protocol !== "https:") return null;
    return url.href.replace(/\/+$/, "");
  } catch {
    return null;
  }
}

/** Chave literal (não vazia) ou `env:NOME` válido. */
export function validateApiKey(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return MSG.keyRequired;
  if (trimmed.startsWith("env:")) {
    return ENV_NAME_RE.test(trimmed.slice(4)) ? null : MSG.keyEnvInvalid;
  }
  return null;
}

export function validateModel(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return MSG.modelRequired;
  if (codePoints(trimmed) > TEXT_MAX_LENGTH || hasControl(trimmed)) return MSG.modelTooLong;
  return null;
}

/** Opções do seletor de protocolo (ordem do radiogroup). */
export const WIRE_API_OPTIONS: readonly { value: WireApi; readonly label: string; readonly text: string }[] = [
  {
    value: "chat",
    label: "Chat Completions",
    get text() {
      return t("settings.protocol.chat.text");
    },
  },
  {
    value: "responses",
    get label() {
      return t("settings.protocol.responses.label");
    },
    get text() {
      return t("settings.protocol.responses.text");
    },
  },
];

/**
 * Protocolo de uma conexão vinda do servidor. Ausente (servidor antigo) ou desconhecido = chat,
 * para não quebrar a tela; só o literal "responses" escolhe Responses.
 */
export function wireApiOf(view: { wire_api?: unknown }): WireApi {
  return view.wire_api === "responses" ? "responses" : "chat";
}

/**
 * Protocolo da conexão do Arquiteto na configuração carregada. `null` quando a configuração ou a
 * conexão não está disponível (a UI então omite a dica de protocolo).
 */
export function architectWireApi(config: ConfigView | null): WireApi | null {
  if (config === null) return null;
  const connection = config.connections.find((c) => c.id === config.agents.architect.connection_id);
  return connection === undefined ? null : wireApiOf(connection);
}

/** Rótulo curto do protocolo para o cartão. */
export function wireApiCardLabel(wireApi: WireApi): string {
  return wireApi === "responses" ? "Responses" : "Chat Completions";
}

export interface ConnectionFormValues {
  id: string;
  name: string;
  base_url: string;
  api_key: string;
}

/**
 * Valida o formulário de conexão. Em edição (`isEdit`), chave vazia mantém a salva e é aceita.
 */
export function validateConnectionForm(values: ConnectionFormValues, isEdit: boolean): FieldErrors {
  const errors: FieldErrors = {};
  const id = validateConnectionId(values.id);
  if (id !== null) errors.id = id;
  const name = validateName(values.name);
  if (name !== null) errors.name = name;
  const url = validateBaseUrl(values.base_url);
  if (url !== null) errors.base_url = url;
  if (values.api_key.trim().length > 0) {
    const key = validateApiKey(values.api_key);
    if (key !== null) errors.api_key = key;
  } else if (!isEdit) {
    errors.api_key = MSG.keyRequired;
  }
  return errors;
}

/** Mensagens fixas para os códigos de `POST /api/config/connections/test`. */
const TEST_ERROR_MESSAGES: Readonly<Record<string, string>> = localized({
  invalid_config: "settings.test.error.invalid_config",
  timeout: "settings.test.error.timeout",
  connect_failed: "settings.test.error.connect_failed",
  tls_failed: "settings.test.error.tls_failed",
  bad_response: "settings.test.error.bad_response",
  unsupported: "settings.test.error.unsupported",
});

/** Texto do erro HTTP do provedor (com o status quando houver), no idioma ativo. */
function providerHttpText(status: number | undefined): string {
  return typeof status === "number" ? t("settings.test.httpStatus", { status }) : t("settings.test.http");
}

/** Texto do resultado do teste no idioma ativo (somente códigos fixos). */
export function describeTestResult(result: TestConnectionResult): string {
  if (result.ok) {
    const base = typeof result.models_count === "number" ? tp("settings.test.okModels", result.models_count) : t("settings.test.ok");
    return typeof result.latency_ms === "number" ? `${base} (${result.latency_ms} ms).` : `${base}.`;
  }
  if (result.error === "http_error") return providerHttpText(result.status);
  const known = result.error !== undefined && Object.hasOwn(TEST_ERROR_MESSAGES, result.error) ? TEST_ERROR_MESSAGES[result.error] : undefined;
  return known ?? t("settings.test.failed");
}

/** Mensagens fixas da listagem de modelos (além das de `TEST_ERROR_MESSAGES`), no idioma ativo. */
export const MODELS_MESSAGES = localized({
  empty: "settings.models.empty",
  truncated: "settings.models.truncated",
  loading: "settings.models.loading",
  notListed: "settings.models.notListed",
});

/** Texto da falha da listagem de modelos no idioma ativo (somente códigos fixos). */
export function describeModelsError(error: string, status?: number): string {
  if (error === "http_error") return providerHttpText(status);
  if (error === "unsupported") return t("settings.models.unsupported");
  if (Object.hasOwn(TEST_ERROR_MESSAGES, error)) return TEST_ERROR_MESSAGES[error] ?? t("settings.models.failed");
  return t("settings.models.failed");
}
