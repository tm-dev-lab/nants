/*
 * Texto humano para falha de `postChat` (turno do Arquiteto). Usado pelo chat e pelo pedido
 * automático da janela Plano: mesma regra, textos fixos de `copy.ts`.
 */
import type { ApiFailure } from "./api/client";
import type { WireApi } from "./api/types";
import { localizeProviderDetail, prereqMissingMessage, providerFailureDetail, providerFailureMessage, providerReasonText } from "./copy";
import { t } from "./i18n";
import { CHAT_FAILED_CODE, providerEvidence, providerSpecificMessage, providerStopText } from "./i18n/providerEvidence";
import { apiFailureText } from "./i18n/serverCodes";

/**
 * Motivo da falha do provedor no idioma ativo (spec 119): pelo `kind` neutro do servidor ou, num
 * `detail` em pt-BR, pelo rótulo lido de volta no dicionário.
 */
function providerDetailOf(error: ApiFailure): string | null {
  const byKind = providerReasonText(error.providerKind, error.providerKindCode);
  if (byKind !== null) return byKind;
  const legacy = providerFailureDetail(error.providerDetail);
  return legacy === null ? null : localizeProviderDetail(legacy);
}

/**
 * 504: o servidor cancelou o turno. Abortado no cliente: o servidor pode ter respondido.
 * Causa fixa do provedor: mensagem específica, com o motivo entre parênteses quando houver; a dica
 * de protocolo depende da conexão do Arquiteto.
 */
export function chatFailureMessage(error: ApiFailure, architectWire: WireApi | null): string {
  // 412 `prereq_missing` (spec 093 T02): o que falta configurar, por rótulos fixos.
  if (error.status === 412 && error.reason === "prereq_missing") return prereqMissingMessage(error.missing ?? []);
  if (error.status === 504) {
    return error.reason === "draft_saved" ? t("chat.timeoutDraftSaved") : t("chat.timeout");
  }
  if (error.kind === "timeout") return t("chat.clientTimeout");
  // Spec 133 T11: com a evidência nos `params`, a orientação específica da causa (plano, limites,
  // ritmo, protocolo) seguida de quem falhou; sem orientação própria, a frase com o papel, a
  // conexão, o modelo e o motivo pela categoria.
  const evidence = error.providerCode !== undefined ? providerEvidence(error.params) : null;
  if (error.providerCode !== undefined && evidence !== null) {
    const specific = providerSpecificMessage(error.providerCode, architectWire);
    if (specific !== null) return `${specific}\n${evidence.who}`;
    const withEvidence = providerStopText(CHAT_FAILED_CODE, error.params, null);
    if (withEvidence !== null) return withEvidence;
  }
  if (error.providerCode !== undefined) {
    return providerFailureMessage(error.providerCode, architectWire, providerDetailOf(error)) ?? t("chat.fallback");
  }
  return apiFailureText(error) || t("chat.fallback");
}
