/*
 * Mensagens do servidor no idioma ativo (spec 102, decisão 2).
 *
 * Erros e mensagens do sistema chegam com `code` estável e `params`; a interface traduz pelo
 * dicionário (`serverError.<code>` e `system.<code>`). Prioridade de um erro da API:
 * 1. `code` com tradução (e todos os parâmetros do texto presentes);
 * 2. `message` do servidor, quando não há `code` traduzível e ela não é um texto de reserva;
 * 3. texto de reserva do cliente (status HTTP, rede, tempo limite) no idioma ativo.
 */
import { messageKeyForText, type ApiFailure } from "../api/client";
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
  const values = withProviderReason(params ?? {}, lang);
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
 * Texto já guardado como string (ex.: `configError`, `loadError`): no idioma ativo quando é um texto
 * pt-BR conhecido (reserva do cliente ou mensagem fixa do servidor); senão, como veio.
 */
export function messageText(text: string, lang: Lang = getLanguage()): string {
  const key = messageKeyForText(text);
  return key === undefined ? text : translate(lang, key);
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
  const fixed = translateCode("system", message.code, message.params, lang);
  if (fixed === null) return message.text;
  if (!SYSTEM_CODES_WITH_TAIL.has(message.code)) return fixed;
  const at = message.text.indexOf("\n");
  return at < 0 ? fixed : fixed + message.text.slice(at);
}

/** Aviso com `code` (ex.: `notice_codes` do app): traduzido, ou `fallback`. */
export function noticeText(notice: { code?: unknown; params?: unknown }, fallback: string, lang: Lang = getLanguage()): string {
  if (typeof notice.code !== "string") return fallback;
  const params = parseCodeParams(notice.params) ?? undefined;
  return translateCode("serverError", notice.code, params, lang) ?? fallback;
}
