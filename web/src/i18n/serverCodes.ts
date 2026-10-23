/*
 * Mensagens do servidor no idioma ativo (spec 102, decisão 2).
 *
 * Erros e mensagens do sistema chegam com `code` estável e `params`; a interface traduz pelo
 * dicionário (`serverError.<code>` e `system.<code>`). Prioridade de um erro da API:
 * 1. `code` com tradução (e todos os parâmetros do texto presentes); desde a spec 115 T02, todo
 *    erro do servidor tem `code`, e o texto do servidor nunca é casado nem repassado;
 * 2. texto de reserva do cliente (status HTTP, rede, tempo limite) no idioma ativo (`messageKey`);
 * 3. `message`, o texto fixo que o próprio cliente montou.
 */
import type { ApiFailure } from "../api/client";
import { parseCodeParams, type CodeParams } from "../api/codes";
import { DICTIONARIES, getLanguage, translate, type Lang, type MessageKey } from "./index";

const PARAM_RE = /\{(\w+)\}/g;

/**
 * Parâmetros montados de uma falha do provedor (spec 102): o servidor manda só valores neutros
 * (`provider`, `kind`, `provider_code`); o rótulo do motivo vem do dicionário (`provider.kind.<kind>`).
 * - `reason`: rótulo do `kind`, com `: <provider_code>` quando houver;
 * - `failure`: `provider`, com ` (<reason>)` quando houver motivo.
 * Valores já presentes nos `params` não são trocados.
 */
function withProviderReason(params: CodeParams, lang: Lang): CodeParams {
  const out: Record<string, string | number> = { ...params };
  const kind = params.kind;
  const providerCode = typeof params.provider_code === "string" ? params.provider_code : null;
  let reason: string | null = null;
  if (typeof kind === "string" && kind !== "") {
    const key = `provider.kind.${kind}`;
    const dict = DICTIONARIES[lang];
    const label = Object.hasOwn(dict, key) ? dict[key as MessageKey] : kind;
    reason = providerCode === null ? label : `${label}: ${providerCode}`;
    if (!Object.hasOwn(out, "reason")) out.reason = reason;
  }
  if (typeof params.provider === "string" && !Object.hasOwn(out, "failure")) {
    out.failure = reason === null ? params.provider : `${params.provider} (${reason})`;
  }
  return out;
}

/** Prefixo das chaves do rótulo do motivo da falha do provedor. */
const PROVIDER_KIND_PREFIX = "provider.kind.";
/** Código saneado do provedor (o mesmo formato do servidor, `ProviderCode`). */
const PROVIDER_REASON_CODE = /^[a-z0-9_.-]{1,40}$/;
/** Texto de `ProviderFailure::describe`: `<provider>` ou `<provider> (<detalhe>)`. */
const DESCRIBED = /^(provider_[a-z0-9_]+)(?: \((.*)\))?$/;

/**
 * Detalhe gravado em pt-BR (`<rótulo>` ou `<rótulo>: <código>`, spec 119) em `lang`: o rótulo é
 * lido de volta pelo dicionário pt-BR (`provider.kind.*`). Fora do vocabulário, como veio.
 */
export function localizeProviderDetailIn(detail: string, lang: Lang): string {
  const pt = DICTIONARIES["pt-BR"];
  const cut = detail.lastIndexOf(": ");
  const candidates: [string, string][] = [[detail, ""]];
  if (cut > 0) candidates.push([detail.slice(0, cut), detail.slice(cut + 2)]);
  for (const [label, code] of candidates) {
    const key = Object.keys(pt).find((k) => k.startsWith(PROVIDER_KIND_PREFIX) && pt[k as MessageKey] === label);
    if (key === undefined || (code !== "" && !PROVIDER_REASON_CODE.test(code))) continue;
    const text = DICTIONARIES[lang][key as MessageKey];
    return code === "" ? text : `${text}: ${code}`;
  }
  return detail;
}

/**
 * `failure` da falha da imagem gravada antes da spec 119 (`params.reason` com o texto de
 * `describe`): o detalhe entre parênteses sai em `lang`; fora do vocabulário, como veio.
 */
function withLegacyImageFailure(params: CodeParams, lang: Lang): CodeParams {
  if (Object.hasOwn(params, "failure") || typeof params.reason !== "string") return params;
  const match = DESCRIBED.exec(params.reason);
  const provider = match?.[1];
  const detail = match?.[2];
  const failure =
    provider === undefined ? params.reason : detail === undefined ? provider : `${provider} (${localizeProviderDetailIn(detail, lang)})`;
  return { ...params, failure };
}

/**
 * `item` do `docker_unavailable` (spec 115 T02): o servidor manda o id do pré-requisito
 * (`docker_cli`, `docker_daemon`...); a tela mostra o nome dele, o mesmo de Primeiros passos
 * (`prereq.machine.<id>`), ou um genérico quando o id não é conhecido.
 */
function withDockerItem(params: CodeParams, lang: Lang): CodeParams {
  const dict = DICTIONARIES[lang];
  const id = params.item;
  const key = `prereq.machine.${String(id)}`;
  const label =
    typeof id === "string" && Object.hasOwn(dict, key) ? dict[key as MessageKey] : dict["prereq.dockerItemUnknown" as MessageKey];
  return { ...params, item: label };
}

/**
 * Famílias de textos com `code`: erros da API, mensagens do sistema, etapas da atividade e rótulos
 * da atividade (ferramentas e avisos fixos do servidor).
 */
type CodePrefix = "serverError" | "system" | "activity.stage" | "activity.code";

/** Texto de `prefix.code` em `lang`, ou `null` sem chave ou com parâmetro do texto faltando. */
export function translateCode(prefix: CodePrefix, code: string, params: CodeParams | undefined, lang: Lang = getLanguage()): string | null {
  const key = `${prefix}.${code}`;
  const dict = DICTIONARIES[lang];
  if (!Object.hasOwn(dict, key)) return null;
  const template = dict[key as MessageKey];
  const base = withProviderReason(params ?? {}, lang);
  const values =
    prefix === "serverError" && code === "docker_unavailable"
      ? withDockerItem(base, lang)
      : prefix === "activity.code" && code === "tool.image_failed"
        ? withLegacyImageFailure(base, lang)
        : base;
  for (const match of template.matchAll(PARAM_RE)) {
    const name = match[1] ?? "";
    if (!Object.hasOwn(values, name)) return null;
  }
  return translate(lang, key as MessageKey, values);
}

/** Texto do erro da API para exibir agora, no idioma ativo (ou em `lang`). */
export function apiFailureText(failure: ApiFailure, lang: Lang = getLanguage()): string {
  if (failure.code !== undefined) {
    const text = translateCode("serverError", failure.code, failure.params, lang);
    if (text !== null) return text;
  }
  if (failure.messageKey !== undefined) return translate(lang, failure.messageKey);
  return failure.message;
}


/**
 * Mensagens do sistema cujo `text` traz, depois da primeira quebra de linha, um conteúdo longo
 * (lista de critérios não atendidos, erro): ele não cabe num param (o servidor corta em 512 bytes)
 * e aparece inteiro, como veio, abaixo da frase fixa traduzida.
 */
const SYSTEM_CODES_WITH_TAIL: ReadonlySet<string> = new Set(["final.unavailable", "final.still_unmet", "final.fix_refused"]);

/** Mensagem gravada na conversa (`agent_message`/diálogo): pelo `code` quando houver; senão, o texto. */
export function systemMessageText(
  message: { text: string; code?: string; params?: CodeParams },
  lang: Lang = getLanguage(),
): string {
  if (message.code === undefined) return message.text;
  if (message.code === "run.relaunch_failed") return relaunchFailedText(message, lang);
  const fixed = translateCode("system", message.code, message.params, lang);
  if (fixed === null) return message.text;
  if (!SYSTEM_CODES_WITH_TAIL.has(message.code)) return fixed;
  const at = message.text.indexOf("\n");
  return at < 0 ? fixed : fixed + message.text.slice(at);
}

/**
 * Relançamento recusado (spec 115 T02): o motivo vem pelo código da recusa (`params.error_code`,
 * com os `params` dela), traduzido como o erro da API; `params.message` (texto em pt-BR) só vale
 * para mensagens antigas, sem código. O `detail` técnico aparece cru numa segunda linha, com o
 * rótulo traduzido.
 */
function relaunchFailedText(message: { text: string; code?: string; params?: CodeParams }, lang: Lang): string {
  const params: Record<string, string | number> = { ...(message.params ?? {}) };
  const errorCode = params.error_code;
  if (typeof errorCode === "string") {
    const reason = translateCode("serverError", errorCode, params, lang);
    if (reason !== null) params.message = reason;
  }
  const fixed = translateCode("system", "run.relaunch_failed", params, lang);
  if (fixed === null) return message.text;
  const detail = params.detail;
  return typeof detail === "string" && detail !== ""
    ? `${fixed}\n${translate(lang, "system.run.relaunch_failed_detail" as MessageKey, { detail })}`
    : fixed;
}

/** Aviso com `code` (ex.: `notice_codes` do app): traduzido, ou `fallback`. */
export function noticeText(notice: { code?: unknown; params?: unknown }, fallback: string, lang: Lang = getLanguage()): string {
  if (typeof notice.code !== "string") return fallback;
  const params = parseCodeParams(notice.params) ?? undefined;
  return translateCode("serverError", notice.code, params, lang) ?? fallback;
}
