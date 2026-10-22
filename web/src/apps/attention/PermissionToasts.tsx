/*
 * Aviso flutuante de permissão (spec 093, decisão 10): cada pedido pendente (de qualquer projeto)
 * vira um aviso persistente com `role="alert"`, as mesmas decisões da Conversa e "Ir para a
 * conversa". Nunca rouba o foco. Empilha até 3 (os mais recentes) com "+N" para o resto e some
 * só quando o pedido é resolvido em qualquer lugar (o pedido sai do store).
 */
import { memo, useMemo } from "react";
import { Icon } from "../../desktop/Icon";
import { useShell } from "../../desktop/shellContext";
import { useT } from "../../i18n/react";
import { useAppStore, type AppState, type PendingPermission } from "../../store";
import { agentLabel } from "../shared/agents";
import "../shared/apps.css";
import { PermissionDecisions } from "../shared/PermissionDecisions";
import { environmentReviewHint, isEnvironmentRequest } from "../shared/permissions";
import { permissionTitle } from "../conversation/PermissionMessage";

export const MAX_PERMISSION_TOASTS = 3;
const PREVIEW_CHARS = 120;

/** Pedidos pendentes de todos os projetos, do mais antigo ao mais novo. */
function pendingAcrossProjects(state: Pick<AppState, "permissions" | "removedProjectIds">): PendingPermission[] {
  const out: PendingPermission[] = [];
  for (const [projectId, list] of Object.entries(state.permissions)) {
    if (state.removedProjectIds[projectId] === true) continue;
    out.push(...list);
  }
  return out.sort((a, b) => (a.at ?? 0) - (b.at ?? 0));
}

/** Os avisos visíveis (até 3, os mais recentes) e quantos ficaram de fora. */
export function stackToasts<T>(items: readonly T[], max = MAX_PERMISSION_TOASTS): { shown: T[]; hidden: number } {
  const shown = items.slice(Math.max(0, items.length - max));
  return { shown, hidden: items.length - shown.length };
}

function PermissionToastsView() {
  const { t, tp } = useT();
  const shell = useShell();
  const permissions = useAppStore((s) => s.permissions);
  const removed = useAppStore((s) => s.removedProjectIds);
  const projects = useAppStore((s) => s.config?.projects);
  const all = useMemo(() => pendingAcrossProjects({ permissions, removedProjectIds: removed }), [permissions, removed]);
  const { shown, hidden } = stackToasts(all);
  if (all.length === 0) return null;
  const nameOf = (id: string): string => projects?.find((p) => p.id === id)?.name ?? id;
  return (
    <div className="nd-permission-toasts" data-permission-toasts="">
      {hidden > 0 ? (
        <p className="nd-toast-more nd-overlay" data-toast-more={hidden}>
          <Icon name="plus" size={14} />
          <span>
            {tp("permissionToasts.more", hidden)}
          </span>
        </p>
      ) : null}
      {shown.map((request) => {
        const command = request.command.length > PREVIEW_CHARS ? request.command.slice(0, PREVIEW_CHARS) : request.command;
        // Mudança de ambiente: só "Ir para a conversa" (o diff é visto lá antes de aprovar).
        const environment = isEnvironmentRequest(request);
        return (
          <div key={`${request.projectId}:${request.requestId}`} role="alert" className="nd-permission-toast nd-overlay" data-permission-toast={request.requestId}>
            <p className="nd-attention-item-title" data-tone="amber">
              <Icon name="bell" size={14} />
              <span>
                {permissionTitle(request)} <span className="nd-muted">{agentLabel(request.agent)}{request.taskId !== null ? ` · ${request.taskId}` : ""} · {nameOf(request.projectId)}</span>
              </span>
            </p>
            {environment ? (
              <p className="nd-muted" data-environment-review="">
                {environmentReviewHint()}
              </p>
            ) : (
              <>
                <pre className="nd-command nd-mono nd-command-clip">{command}</pre>
                <PermissionDecisions projectId={request.projectId} request={request} compact onDecided={shell.refreshAttention} />
              </>
            )}
            <div className="nd-actions">
              <button
                type="button"
                className="nd-btn nd-btn-sm"
                data-variant={environment ? undefined : "ghost"}
                onClick={() => shell.goTo(request.projectId, "conversation")}
              >
                <Icon name="conversation" size={14} />
                {t("permissionToasts.goToConversation")}
              </button>
            </div>
          </div>
        );
      })}
    </div>
  );
}

export const PermissionToasts = memo(PermissionToastsView);
