/*
 * Decisões honestas de um pedido de permissão, compartilhadas pela Conversa, pelo aviso flutuante e pelo "Precisa de você" (spec 093, T06):
 * - "Permitir uma vez" sempre; "Permitir nesta execução" só para Desenvolvedor e Revisor (no chat
 *   do Arquiteto `run` valeria `once`); "Sempre permitir" só com `always_allowed` do servidor;
 *   "Negar" sempre. Mudança de ambiente: só "Aprovar mudança" e "Negar".
 * - A confirmação usa o `applied` da resposta; 404 (já resolvido ou expirado) vira resolvido.
 */
import { resolvePermission } from "../../api/client";
import type { PermissionDecision } from "../../api/types";
import {
  PERMISSION_ALWAYS_FALLBACK_NOTICE,
  PERMISSION_APPLIED_NOTICE,
  PERMISSION_ENV_OPTION_HINT,
  PERMISSION_OPTION_HINT,
  PERMISSION_OPTION_LABEL,
  permissionAlwaysReason,
} from "../../copy";
import { t } from "../../i18n";
import { apiFailureText } from "../../i18n/serverCodes";
import { appStore, type ResolvedPermission } from "../../store";

/** Pedido mínimo para decidir (o do store ou o do `/api/attention`). */
export interface DecidableRequest {
  requestId: string;
  agent: string;
  kind?: string | null;
  alwaysAllowed?: boolean;
  alwaysDeniedReason?: string | null;
}

/**
 * Mudança de ambiente fora da Conversa (sino e aviso flutuante), no idioma ativo: a ação é "Ir para a conversa",
 * para o diff ser visto antes de aprovar.
 */
export function environmentReviewHint(): string {
  return t("permission.environmentReview");
}

export function isEnvironmentRequest(request: DecidableRequest): boolean {
  return request.kind === "environment";
}

/** Opções oferecidas, na ordem dos botões. */
export function permissionOptions(request: DecidableRequest): PermissionDecision[] {
  const env = isEnvironmentRequest(request);
  const canAlways = !env && request.alwaysAllowed === true;
  const canRun = !env && (request.agent === "developer" || request.agent === "reviewer");
  return ["once", ...(canRun ? (["run"] as const) : []), ...(canAlways ? (["always"] as const) : []), "deny"];
}

export function optionLabel(request: DecidableRequest, decision: PermissionDecision): string {
  return isEnvironmentRequest(request) && decision === "once" ? t("permission.approveChange") : PERMISSION_OPTION_LABEL[decision];
}

export function optionHint(request: DecidableRequest, decision: PermissionDecision): string {
  return isEnvironmentRequest(request) && (decision === "once" || decision === "deny")
    ? PERMISSION_ENV_OPTION_HINT[decision]
    : PERMISSION_OPTION_HINT[decision];
}

/** Por que não há "Sempre" (só motivos conhecidos do servidor; o resto vira o genérico). */
export function alwaysDeniedExplanation(request: DecidableRequest): string | null {
  if (isEnvironmentRequest(request) || request.alwaysAllowed === true) return null;
  return permissionAlwaysReason(request.alwaysDeniedReason);
}

function isDecision(value: unknown): value is PermissionDecision {
  return value === "once" || value === "run" || value === "always" || value === "deny";
}

/** Texto do resultado de um pedido resolvido. */
export function outcomeText(entry: ResolvedPermission): string {
  if (entry.applied === null) return t("permission.closed");
  const requested = entry.requested ?? entry.applied;
  if (!isDecision(entry.applied)) return t("permission.closed");
  if (requested === "always" && (entry.applied === "once" || entry.applied === "run")) return PERMISSION_ALWAYS_FALLBACK_NOTICE[entry.applied];
  return PERMISSION_APPLIED_NOTICE[entry.applied];
}

/** Sem pedido pendente no projeto, a fase "aguardando aprovação" volta a ser a derivada. */
function releaseWaitingPhase(projectId: string): void {
  const state = appStore.getState();
  if ((state.permissions[projectId] ?? []).length > 0) return;
  if (state.phases[projectId] !== "waiting_approval") return;
  appStore.setState((s) => {
    const phases = { ...s.phases };
    delete phases[projectId];
    return { phases };
  });
}

/** Resultado de `decidePermission`: `null` = resolvido (inclusive o 404); texto = erro. */
export async function decidePermission(projectId: string, requestId: string, decision: PermissionDecision): Promise<string | null> {
  const result = await resolvePermission(projectId, requestId, decision);
  if (result.ok) {
    appStore.settlePermission(projectId, requestId, result.data.applied, decision);
    releaseWaitingPhase(projectId);
    return null;
  }
  if (result.error.status === 404) {
    appStore.settlePermission(projectId, requestId, null);
    releaseWaitingPhase(projectId);
    return null;
  }
  return apiFailureText(result.error) || t("permission.fallback");
}
