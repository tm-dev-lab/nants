/*
 * Textos da atividade ao vivo e da trilha no idioma ativo (spec 102): o servidor manda `code` e
 * `params` neutros junto do texto de reserva em pt-BR (etapa, rótulo de ferramenta, avisos fixos
 * do limitador). Sem `code` (registros antigos da trilha, etapa desconhecida), o texto aparece
 * como veio.
 */
import type { Coded } from "../../api/codes";
import type { ToolActionItem } from "../../api/types";
import { getLanguage, t, type MessageKey } from "../../i18n";
import { translateCode } from "../../i18n/serverCodes";
import { localizeProviderDetail } from "../../copy";
import { FINAL_VERIFICATION_STAGE, type ActivityEntry } from "../../store";

/** Rótulo de `activity.code.<code>` no idioma ativo, ou `null` (sem `code`, sem chave ou sem parâmetro). */
function activityCodeText(coded: Coded | undefined): string | null {
  return coded === undefined ? null : translateCode("activity.code", coded.code, coded.params);
}

/** Etapa no idioma ativo: pelo `code`; sem ele, a verificação final antiga pela marca, ou o texto. */
export function stageText(entry: Partial<Pick<ActivityEntry, "stage" | "stageCode">>): string | null {
  if (entry.stage === undefined) return null;
  const coded = entry.stageCode;
  const byCode = coded === undefined ? null : translateCode("activity.stage", coded.code, coded.params);
  if (byCode !== null) return byCode;
  return entry.stage === FINAL_VERIFICATION_STAGE ? t("finalVerification.title") : entry.stage;
}

/** Último rótulo de ferramenta no idioma ativo (ou o texto de reserva). */
export function toolText(entry: Pick<ActivityEntry, "tool"> & Partial<Pick<ActivityEntry, "toolCode">>): string {
  return activityCodeText(entry.toolCode) ?? entry.tool;
}

/**
 * Texto do passo no idioma ativo: aviso fixo pelo `code`, a nova tentativa pela contagem, ou o
 * texto como veio (o que o agente escreve).
 */
export function stepText(entry: Pick<ActivityEntry, "text"> & Partial<Pick<ActivityEntry, "textCode">>): string {
  const byCode = activityCodeText(entry.textCode);
  if (byCode !== null) return byCode;
  return entry.text.includes(RETRY_MARK) ? retryDetail(entry.text) : entry.text;
}

/**
 * Rótulos das chamadas de saída estruturada (spec 121), pelo código que o servidor manda em
 * `structured` (`agents::activity::structured`). Código desconhecido usa o rótulo genérico.
 */
const STRUCTURED_KEYS: Readonly<Record<string, MessageKey>> = {
  plan_review: "activity.structured.plan_review",
  verdict: "activity.structured.verdict",
  final_check: "activity.structured.final_check",
  diagnosis: "activity.structured.diagnosis",
  recovery: "activity.structured.recovery",
  summary: "activity.structured.summary",
};

/** Rótulo do passo que escreve uma resposta estruturada (spec 121), ou `null` sem código. */
export function structuredText(entry: Partial<Pick<ActivityEntry, "structured">>): string | null {
  const code = entry.structured;
  if (code === undefined) return null;
  return t(Object.hasOwn(STRUCTURED_KEYS, code) ? (STRUCTURED_KEYS[code] as MessageKey) : "activity.structured.default");
}

/** Texto que começa como JSON (`{`, `[` ou bloco de código JSON): a reserva da prévia (spec 121). */
const JSON_START = /^(?:[{[]|```[ \t]*(?:json[ \t]*(?:\r?\n|$)|\r?\n[ \t]*[{[]))/i;

/** O texto ao vivo parece saída estruturada (reserva sem código; só para a prévia da Atividade). */
export function looksStructured(text: string): boolean {
  return JSON_START.test(text.trim());
}

/**
 * Texto ao vivo do passo (spec 121): aviso fixo pelo `code` primeiro; depois, na chamada de saída
 * estruturada, o rótulo do tipo dela, nunca o JSON; senão, o texto como veio.
 */
export function draftText(entry: Pick<ActivityEntry, "text"> & Partial<Pick<ActivityEntry, "textCode" | "structured">>): string {
  if (entry.textCode !== undefined) return stepText(entry);
  return structuredText(entry) ?? stepText(entry);
}

/** O passo está esperando o limite do provedor (limitador ou limite do provedor, spec 089). */
export function isPacing(entry: Partial<Pick<ActivityEntry, "textCode">>): boolean {
  const code = entry.textCode?.code;
  return code === "pace.waiting" || code === "pace.throttled";
}

/** Rótulo de uma ação da trilha (`tools.jsonl`) no idioma ativo; linha antiga sem `code`, como veio. */
export function toolActionText(item: ToolActionItem): string {
  if (item.code === undefined) return item.text;
  return activityCodeText({ code: item.code, params: item.params ?? {} }) ?? item.text;
}

/** Marca do aviso de nova tentativa do servidor (`llm_client::retry_notice`, sempre em pt-BR). */
export const RETRY_MARK = "repetindo (";
const RETRY_COUNT = /repetindo \((\d+)\/(\d+)\)/;
/** Aviso de nova tentativa inteiro (texto de reserva do servidor): motivo, tentativa e total. */
const RETRY_NOTICE = /^resposta do provedor falhou \((.*)\); repetindo \((\d+)\/(\d+)\)$/;
/** Código da nova tentativa (spec 119): `params.kind`/`provider_code`, `attempt` e `total`. */
export const RETRY_CODE = "provider.retry";

/**
 * Detalhe do aviso de nova tentativa (spec 088), sem `code` (registros antigos): em pt-BR, o aviso
 * como veio; nos outros idiomas, o aviso montado pelo dicionário com o motivo lido de volta (spec
 * 119) ou, motivo fora do vocabulário, só a contagem ("attempt 1 of 2").
 */
export function retryDetail(notice: string): string {
  if (getLanguage() === "pt-BR") return notice;
  const full = RETRY_NOTICE.exec(notice.trim());
  if (full !== null) {
    const reason = localizeProviderDetail(full[1] ?? "");
    if (reason !== full[1]) return t("activity.code.provider.retry", { reason, attempt: full[2] ?? "", total: full[3] ?? "" });
  }
  const match = RETRY_COUNT.exec(notice);
  return match === null ? "" : t("activity.retry.detail", { attempt: match[1] ?? "", total: match[2] ?? "" });
}
