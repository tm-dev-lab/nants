/*
 * Botões das decisões honestas de um pedido de permissão (ver `permissions.ts`), com a linha do
 * efeito de cada opção (`aria-describedby`), o motivo de não haver "Sempre" e o erro do pedido.
 * O mesmo componente serve a Conversa, o aviso flutuante e o "Precisa de você": decidir em
 * qualquer um resolve todos (o pedido sai do store).
 */
import { useId, useState } from "react";
import type { PermissionDecision } from "../../api/types";
import { useT } from "../../i18n/react";
import {
  alwaysDeniedExplanation,
  decidePermission,
  isEnvironmentRequest,
  optionHint,
  optionLabel,
  permissionOptions,
  type DecidableRequest,
} from "./permissions";

interface PermissionDecisionsProps {
  projectId: string;
  request: DecidableRequest;
  /** Compacto (aviso flutuante): as linhas de efeito ficam só para leitores de tela. */
  compact?: boolean;
  onDecided?: () => void;
}

export function PermissionDecisions({ projectId, request, compact = false, onDecided }: PermissionDecisionsProps) {
  const hintId = useId();
  const { t } = useT();
  const reasonId = useId();
  const [busy, setBusy] = useState<PermissionDecision | null>(null);
  const [error, setError] = useState<string | null>(null);
  const options = permissionOptions(request);
  const reason = alwaysDeniedExplanation(request);
  const env = isEnvironmentRequest(request);

  async function decide(decision: PermissionDecision): Promise<void> {
    if (busy !== null) return;
    setBusy(decision);
    setError(null);
    const problem = await decidePermission(projectId, request.requestId, decision);
    setBusy(null);
    if (problem !== null) {
      setError(problem);
      return;
    }
    onDecided?.();
  }

  return (
    <div className="nd-decisions" data-compact={compact ? "true" : undefined}>
      <div
        role="group"
        aria-label={env ? t("permission.groupEnvironment") : t("permission.groupCommand")}
        aria-describedby={reason !== null ? reasonId : undefined}
        aria-busy={busy !== null}
        className="nd-decision-row"
      >
        {options.map((decision) => (
          <button
            key={decision}
            type="button"
            className="nd-btn nd-btn-sm"
            data-variant={decision === "deny" ? "ghost" : undefined}
            data-decision={decision}
            disabled={busy !== null}
            aria-describedby={`${hintId}-${decision}`}
            onClick={() => void decide(decision)}
          >
            {busy === decision ? t("permission.sending") : optionLabel(request, decision)}
          </button>
        ))}
      </div>
      <div data-testid="permission-hints" className={compact ? "nd-sr-only" : "nd-decision-hints"}>
        {options.map((decision) => (
          <p key={decision} id={`${hintId}-${decision}`}>
            {optionHint(request, decision)}
          </p>
        ))}
      </div>
      {reason !== null ? (
        <p id={reasonId} className={compact ? "nd-sr-only" : "nd-decision-reason"}>
          {reason}
        </p>
      ) : null}
      {error !== null ? (
        <p role="alert" className="nd-dialog-error">
          {error}
        </p>
      ) : null}
    </div>
  );
}
