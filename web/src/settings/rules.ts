/*
 * Regras puras de Conexões & Agentes (sem React, testáveis).
 * Espelham as validações do servidor (config_api.rs) para feedback imediato; o servidor
 * continua sendo a autoridade. Mensagens são fixas e saem do dicionário no idioma ativo (spec 102
 * T05): as tabelas usam `localized` ou getters (cada leitura devolve o texto do idioma atual).
 */
import type { ConfigView, ConnectionKind, Effort, ModelEntry, ModelPricing, TestConnectionResult, WireApi } from "../api/types";
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
  plan_limit: "settings.test.error.plan_limit",
  plan_not_eligible: "settings.test.error.plan_not_eligible",
  plan_needs_login: "settings.test.error.plan_needs_login",
  connection_kind_not_supported_yet: "settings.test.error.connection_kind_not_supported_yet",
  // Spec 132 T08: Anthropic e Google Gemini.
  auth_invalid: "settings.test.error.auth_invalid",
  forbidden: "settings.test.error.forbidden",
  model_not_found: "settings.test.error.model_not_found",
  rate_limited: "settings.test.error.rate_limited",
  spend_limit: "settings.test.error.spend_limit",
  daily_quota: "settings.test.error.daily_quota",
  billing: "settings.test.error.billing",
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

// --- Conexões nativas (spec 132 T10): Anthropic e Google Gemini ---

/** Tipos com API própria: endereço oficial fixo, protocolo implícito e só nome e chave na tela. */
export type NativeKind = "anthropic" | "google_gemini";

export function isNativeKind(kind: unknown): kind is NativeKind {
  return kind === "anthropic" || kind === "google_gemini";
}

/**
 * Dados fixos de cada provedor nativo: o nome do provedor (marca, igual nos dois idiomas), o
 * endereço oficial (o mesmo de `src/config/schema.rs`, mostrado na dica do endereço avançado), a
 * página oficial onde se cria a chave e o identificador base da conexão nova.
 */
export const NATIVE_PROVIDERS: Readonly<Record<NativeKind, { brand: string; officialUrl: string; keyUrl: string; idBase: string }>> = {
  anthropic: {
    brand: "Anthropic",
    officialUrl: "https://api.anthropic.com",
    keyUrl: "https://console.anthropic.com/settings/keys",
    idBase: "anthropic",
  },
  google_gemini: {
    brand: "Google Gemini",
    officialUrl: "https://generativelanguage.googleapis.com",
    keyUrl: "https://aistudio.google.com/apikey",
    idBase: "gemini",
  },
};

/**
 * O endereço é o do Gemini pela camada compatível com a OpenAI (`.../v1beta/openai/`)? A conexão
 * por chave mostra a nota de que ela funciona com limites e de que o caminho recomendado é a
 * conexão Gemini nativa (spec 132).
 */
export function isGeminiCompatUrl(value: string): boolean {
  try {
    const url = new URL(value.trim());
    return url.hostname === new URL(NATIVE_PROVIDERS.google_gemini.officialUrl).hostname && /\/openai(\/|$)/.test(url.pathname);
  } catch {
    return false;
  }
}

/** Página do console da Anthropic onde se ajusta o teto de gasto (pausa `run.spend_limit`). */
export const ANTHROPIC_LIMITS_URL = "https://console.anthropic.com/settings/limits";

/**
 * Tipo da conexão vinda do servidor. Ausente (servidor antigo) ou desconhecido = por chave, para não
 * quebrar a tela.
 */
export function connectionKindOf(view: { kind?: unknown }): ConnectionKind {
  const kind = view.kind;
  return kind === "chatgpt_plan" || kind === "anthropic" || kind === "google_gemini" ? kind : "api_key";
}

/**
 * Identificador da conexão nativa nova, derivado do nome (a tela só pede o nome e a chave): letras
 * sem acento em minúsculas, dígitos, `-` e `_`, até 64 caracteres. Vazio usa a base do provedor.
 * Já usado, ganha o sufixo `-2`, `-3`... (o servidor recusa id repetido em conexão nova).
 */
export function nativeConnectionId(name: string, kind: NativeKind, existingIds: readonly string[]): string {
  const slug = name
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, ID_MAX_LENGTH - 4)
    .replace(/-+$/g, "");
  const base = slug === "" ? NATIVE_PROVIDERS[kind].idBase : slug;
  if (!existingIds.includes(base)) return base;
  for (let n = 2; ; n += 1) {
    const candidate = `${base}-${n}`;
    if (!existingIds.includes(candidate)) return candidate;
  }
}

export interface NativeFormValues {
  name: string;
  api_key: string;
  /** Endereço avançado (recolhido); vazio usa o oficial. */
  base_url_override: string;
}

export type NativeFieldErrors = Partial<Record<"name" | "api_key" | "base_url_override", string>>;

/** Valida o formulário da conexão nativa. Em edição (`isEdit`), chave vazia mantém a salva. */
export function validateNativeForm(values: NativeFormValues, isEdit: boolean): NativeFieldErrors {
  const errors: NativeFieldErrors = {};
  const name = validateName(values.name);
  if (name !== null) errors.name = name;
  if (values.api_key.trim().length > 0) {
    const key = validateApiKey(values.api_key);
    if (key !== null) errors.api_key = key;
  } else if (!isEdit) {
    errors.api_key = MSG.keyRequired;
  }
  if (values.base_url_override.trim() !== "") {
    const url = validateBaseUrl(values.base_url_override);
    if (url !== null) errors.base_url_override = url;
  }
  return errors;
}

/**
 * Candidatos da sugestão de modelo por papel (spec 132 T10, critério 3): os modelos com raciocínio
 * (`thinking`) e com imagem na entrada (`vision`), na ordem do provedor. Os papéis precisam dos
 * dois: o Arquiteto planeja, o Desenvolvedor trabalha num laço longo de ferramentas e o Revisor
 * confere capturas de tela. Cada filtro só vale quando a lista informa o campo em algum modelo e
 * deixa ao menos um: sem o dado, ou sem nenhum que passe, o filtro é ignorado (primeiro o de
 * imagem, depois o de raciocínio).
 */
export function suggestionCandidates(models: readonly ModelEntry[]): ModelEntry[] {
  const valid = models.filter((m) => typeof m?.id === "string" && m.id.trim() !== "");
  const narrow = (list: readonly ModelEntry[], field: "thinking" | "vision"): readonly ModelEntry[] => {
    if (!list.some((m) => typeof m[field] === "boolean")) return list;
    const kept = list.filter((m) => m[field] === true);
    return kept.length > 0 ? kept : list;
  };
  return [...narrow(narrow(valid, "thinking"), "vision")];
}

/**
 * Sugestão de modelo por papel para a primeira conexão de um provedor (spec 132 T10, critério 3).
 * Regra agnóstica, sem nome de modelo no código: só os dados da lista do provedor e o catálogo de
 * preços.
 *
 * 1. Candidatos: `suggestionCandidates` (raciocínio e imagem).
 * 2. Preço: entre os candidatos com preço no catálogo (`prices`, a estimativa pelo tipo da conexão,
 *    a mesma da tela de Preços), ganha o de menor entrada mais saída por 1M de tokens. Empate fica
 *    com a ordem do provedor.
 * 3. Sem preço para nenhum: o primeiro candidato, na ordem do provedor.
 *
 * Os três papéis recebem o mesmo modelo (o de menor custo atende a todos, como o usuário roda
 * hoje), e o esforço continua o sugerido por papel. A tela deixa trocar cada um. Lista vazia dá
 * `null`.
 */
export function suggestRoleModels(models: readonly ModelEntry[], prices: Readonly<Record<string, ModelPricing>> = {}): Record<AgentRole, string> | null {
  const candidates = suggestionCandidates(models);
  const costOf = (id: string): number | null => {
    const p = Object.hasOwn(prices, id) ? prices[id] : undefined;
    if (p === undefined || typeof p.input_per_mtok !== "number" || typeof p.output_per_mtok !== "number") return null;
    const total = p.input_per_mtok + p.output_per_mtok;
    return Number.isFinite(total) ? total : null;
  };
  let best: ModelEntry | undefined;
  let bestCost: number | null = null;
  for (const model of candidates) {
    const cost = costOf(model.id);
    if (cost !== null && (bestCost === null || cost < bestCost)) {
      best = model;
      bestCost = cost;
    }
  }
  const pick = best ?? candidates[0];
  if (pick === undefined) return null;
  return { architect: pick.id, developer: pick.id, reviewer: pick.id };
}

/** Dica curta do esforço pelo tipo da conexão do papel (spec 132 T10, critério 4), no idioma ativo. */
export function effortHint(kind: ConnectionKind): string {
  switch (kind) {
    case "anthropic":
      return t("settings.agents.effortHint.anthropic");
    case "google_gemini":
      return t("settings.agents.effortHint.google_gemini");
    case "chatgpt_plan":
      return t("settings.agents.effortHint.chatgpt_plan");
    default:
      return t("settings.agents.effortHint.api_key");
  }
}
