/*
 * Indicador da verificação final:
 * enquanto o Revisor confere a spec inteira, "Verificação final em andamento" com a explicação.
 * Vem da atividade ao vivo com `stage: "final_verification"` e some quando ela termina.
 */
import { Icon } from "../../desktop/Icon";
import { useT } from "../../i18n/react";
import { isFinalVerificationActive, useAppStore } from "../../store";

function useFinalVerification(projectId: string): boolean {
  return useAppStore((s) => isFinalVerificationActive(s.activities[projectId]));
}

/**
 * `variant="card"` (Conversa, 095/T04): a mesma linguagem de cartão das mensagens, com o selo do
 * Revisor; as outras janelas mantêm a faixa.
 */
export function FinalVerificationNotice({ projectId, variant = "strip" }: { projectId: string; variant?: "strip" | "card" }) {
  const active = useFinalVerification(projectId);
  const { t } = useT();
  if (!active) return null;
  if (variant === "card") {
    return (
      <div role="status" data-testid="final-verification-strip" className="nd-chat-card nd-final-card">
        <div className="nd-chat-card-head">
          <Icon name="sparkles" size={14} className="nd-final-card-icon" />
          <span className="nd-strong">{t("finalVerification.title")}</span>
        </div>
        <p className="nd-chat-card-body nd-muted">{t("finalVerification.hint")}</p>
      </div>
    );
  }
  return (
    <div role="status" data-testid="final-verification-strip" className="nd-strip" data-tone="violet">
      <Icon name="sparkles" size={14} />
      <span className="nd-strong">{t("finalVerification.title")}</span>
      <span className="nd-muted">{t("finalVerification.hint")}</span>
    </div>
  );
}
