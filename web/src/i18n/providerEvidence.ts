/*
 * Evidência da falha do provedor na tela (spec 133 T01).
 *
 * As mensagens do sistema que param a execução pelo provedor (`run.provider_failed*` e as pausas
 * do plano e dos limites) chegam com `params` neutros: o papel, a conexão, o modelo, a tarefa, a
 * tentativa, a categoria do Nants e os códigos crus do provedor. A frase principal diz o papel, a
 * conexão, o modelo e o motivo traduzido pela categoria; os códigos crus ficam num detalhe
 * recolhível (`raw`), nunca no texto principal.
 */
import type { CodeParams } from "../api/codes";
import { DICTIONARIES, getLanguage, translate, type Lang, type MessageKey } from "./index";

/**
 * `code` das mensagens do sistema que param a execução pelo provedor e, desde a spec 133 T11, a
 * falha do provedor num turno da conversa (`chat.provider_failed`), com a mesma evidência.
 */
export const PROVIDER_STOP_CODES: ReadonlySet<string> = new Set([
  "chat.provider_failed",
  "run.provider_failed",
  "run.provider_failed_detail",
  "run.plan_limit",
  "run.plan_not_eligible",
  "run.plan_needs_login",
  "run.spend_limit",
  "run.daily_quota",
  "run.billing",
]);

/** As que só dizem "o provedor falhou": a frase com a evidência substitui o texto delas. */
const FAILED_CODES: ReadonlySet<string> = new Set(["chat.provider_failed", "run.provider_failed", "run.provider_failed_detail"]);

/** Categorias do Nants (o mesmo vocabulário do servidor, `FailureCategory`). */
const CATEGORIES: ReadonlySet<string> = new Set(["auth", "billing", "quota", "rate", "bad_request", "context_overflow", "server", "transport"]);

/** Campos crus do detalhe recolhível, na ordem exibida, com o rótulo de cada um. */
const RAW_FIELDS: readonly (readonly [string, MessageKey])[] = [
  ["provider", "provider.evidence.provider"],
  ["kind", "provider.evidence.reasonKind"],
  ["provider_code", "provider.evidence.reasonCode"],
  ["status", "provider.evidence.status"],
  ["error_type", "provider.evidence.type"],
  ["error_code", "provider.evidence.code"],
  ["error_param", "provider.evidence.param"],
  ["request_id", "provider.evidence.request"],
  ["connection_kind", "provider.evidence.kind"],
  ["attempt", "provider.evidence.attempt"],
];

interface ProviderEvidenceView {
  /** Papel, tarefa (quando houver), conexão e modelo, numa frase. */
  who: string;
  /**
   * Motivo no idioma ativo: o detalhado da resposta ruim (`provider.kind.<kind>`, com o
   * `provider_code`), quando houver; senão, o da categoria.
   */
  reason: string;
  /** Códigos crus do provedor e do Nants, para o detalhe recolhível. */
  raw: { label: string; value: string }[];
}

function text(params: CodeParams, name: string): string | null {
  const v = params[name];
  if (typeof v === "number") return String(v);
  return typeof v === "string" && v.trim() !== "" ? v : null;
}

function label(lang: Lang, key: string, fallback: string): string {
  const dict = DICTIONARIES[lang];
  return Object.hasOwn(dict, key) ? dict[key as MessageKey] : fallback;
}

/** Motivo detalhado da resposta ruim (`params.kind`, com o `provider_code`), como no texto sem evidência. */
function kindReason(params: CodeParams, lang: Lang): string | null {
  const kind = text(params, "kind");
  if (kind === null) return null;
  const reason = label(lang, `provider.kind.${kind}`, kind);
  const code = text(params, "provider_code");
  return code === null ? reason : `${reason}: ${code}`;
}

/** Evidência da falha nos `params`, ou `null` sem papel (mensagens anteriores à spec 133). */
export function providerEvidence(params: CodeParams | undefined, lang: Lang = getLanguage()): ProviderEvidenceView | null {
  if (params === undefined) return null;
  const role = text(params, "role");
  if (role === null) return null;
  const roleLabel = label(lang, `role.${role}`, role);
  const connection = text(params, "connection_name") ?? text(params, "connection_id") ?? translate(lang, "provider.evidence.noConnection");
  const model = text(params, "model") ?? "?";
  const task = text(params, "task");
  const who =
    task === null
      ? translate(lang, "provider.who", { role: roleLabel, connection, model })
      : translate(lang, "provider.whoTask", { role: roleLabel, task, connection, model });
  const category = text(params, "category");
  const reason =
    kindReason(params, lang) ??
    (category !== null && CATEGORIES.has(category) ? translate(lang, `provider.category.${category}` as MessageKey) : translate(lang, "provider.category.unknown"));
  const raw = RAW_FIELDS.flatMap(([name, key]) => {
    const value = text(params, name);
    return value === null ? [] : [{ label: translate(lang, key), value }];
  });
  return { who, reason, raw };
}

/**
 * Texto da mensagem do sistema com a evidência: para "o provedor falhou", a frase com o papel, a
 * conexão, o modelo e o motivo; para as pausas, o texto delas (`fixed`) seguido de quem parou.
 * `null` quando a mensagem não tem evidência.
 */
export function providerStopText(code: string, params: CodeParams | undefined, fixed: string | null, lang: Lang = getLanguage()): string | null {
  if (!PROVIDER_STOP_CODES.has(code)) return null;
  const ev = providerEvidence(params, lang);
  if (ev === null) return null;
  // Spec 133 T11 (F-01): a falha da conversa com orientação própria para a causa (plano, limites,
  // ritmo) mostra essa orientação e, embaixo, quem falhou; a frase pela categoria fica para o resto.
  const specific = code === CHAT_FAILED_CODE ? providerSpecificMessage(text(params ?? {}, "provider"), null, lang) : null;
  if (specific !== null) return `${specific}\n${ev.who}`;
  if (FAILED_CODES.has(code) || fixed === null) return translate(lang, "provider.failedBy", { who: ev.who, reason: ev.reason });
  return `${fixed}\n${ev.who}`;
}

/** `code` da falha do provedor num turno da conversa (`server/chat.rs`). */
export const CHAT_FAILED_CODE = "chat.provider_failed";

/** Causas fixas com orientação própria, independente do status HTTP. */
const SPECIFIC_KEYS: Readonly<Record<string, MessageKey>> = {
  provider_plan_limit: "providerMsg.planLimit",
  provider_plan_not_eligible: "providerMsg.planNotEligible",
  provider_plan_needs_login: "providerMsg.planNeedsLogin",
  provider_spend_limit: "providerMsg.spendLimit",
  provider_daily_quota: "providerMsg.dailyQuota",
  provider_billing: "providerMsg.billing",
};

/**
 * Orientação específica de uma causa fixa do provedor, no idioma `lang` (as mesmas frases do
 * `providerFailureMessage` de `copy.ts`): o plano ChatGPT, o teto de gasto, a cota diária, a
 * cobrança, o 429 com o conselho de espera e, com conexão `chat`, o 4xx com a dica do protocolo.
 * `null` quando a causa só tem a frase genérica: aí vale a frase pela categoria.
 */
export function providerSpecificMessage(provider: string | null, architectWireApi: string | null, lang: Lang = getLanguage()): string | null {
  if (provider === null) return null;
  const key = SPECIFIC_KEYS[provider];
  if (key !== undefined) return translate(lang, key);
  const http = /^provider_http_([1-5][0-9]{2})$/.exec(provider);
  if (http === null) return null;
  const status = Number(http[1]);
  if (status === 429) return `${translate(lang, "providerMsg.throttled")} ${translate(lang, "providerMsg.throttleAdvice")}`;
  if (architectWireApi === "chat" && status >= 400 && status < 500 && ![401, 403, 404].includes(status)) {
    return `${translate(lang, "providerMsg.refused", { status })} ${translate(lang, "providerMsg.responsesHint")}`;
  }
  return null;
}
