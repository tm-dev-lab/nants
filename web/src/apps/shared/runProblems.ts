/*
 * Falhas de aprovar, retomar e escolher a recuperação em texto humano (Plano e execução
 * pausada). O 412 `prereq_missing` vira a lista do que
 * falta, com a aba de cada item (o "Configurar" leva até ela).
 */
import { isAgentNotConfigured, type ApiFailure } from "../../api/client";
import type { Readiness, ReadinessItem } from "../../desktop/api";
import { APPROVE_REASON_MESSAGES, RECOVERY_REASON_MESSAGES } from "../../copy";
import { t } from "../../i18n";
import { apiFailureText } from "../../i18n/serverCodes";
import { prereqItems, prereqMessage } from "./readiness";

export interface RunProblem {
  message: string;
  /** Pendências a configurar (412 ou agentes ausentes), cada uma com "Configurar". */
  missing: ReadinessItem[] | null;
  /**
   * Veio de um 412 `prereq_missing`: a mensagem e os rótulos são refeitos com a prontidão atual
   * (`ProblemNotice`), para os ids com parâmetro aparecerem com o rótulo do servidor.
   */
  prereq?: boolean;
}

/** Pendência "Agentes" (rótulo no idioma ativo). */
function agentItems(): ReadinessItem[] {
  return [{ id: "agent_developer", label: t("prereq.agents"), settings_tab: "agents" }];
}

/** Texto do erro do servidor no idioma ativo, ou a reserva. */
function failureText(failure: ApiFailure, fallback: string): string {
  return apiFailureText(failure) || fallback;
}

/** Falha de `approve`/`resume` (códigos fixos; nunca o corpo cru). */
export function approveProblem(failure: ApiFailure, readiness: Readiness | null = null): RunProblem {
  const prereq = prereqItems(failure, readiness);
  if (prereq !== null) return { message: prereqMessage(prereq), missing: prereq, prereq: true };
  if (isAgentNotConfigured(failure)) {
    return { message: t("approve.agents"), missing: agentItems() };
  }
  if (failure.status === 409) {
    const human = failure.reason !== undefined ? APPROVE_REASON_MESSAGES[failure.reason] : undefined;
    return { message: human ?? failureText(failure, t("approve.fallback")), missing: null };
  }
  return { message: failureText(failure, t("approve.fallback")), missing: null };
}

/** Falha de `recovery-choice`. */
export function recoveryProblem(failure: ApiFailure, readiness: Readiness | null = null): RunProblem {
  const prereq = prereqItems(failure, readiness);
  if (prereq !== null) return { message: prereqMessage(prereq), missing: prereq, prereq: true };
  if (failure.status === 409) {
    const human = failure.reason !== undefined ? RECOVERY_REASON_MESSAGES[failure.reason] : undefined;
    return { message: human ?? t("recovery.stale"), missing: null };
  }
  if (failure.status === 422) return { message: t("recovery.invalidGuidance"), missing: null };
  if (failure.status === 404) return { message: t("recovery.taskNotFound"), missing: null };
  return { message: failureText(failure, t("recovery.fallback")), missing: null };
}
