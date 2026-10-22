/*
 * Conteúdo da janela Plano (`DEFAULT_APPS.plan.Content`, em `memo`): o plano da spec ativa
 * (`PlanCard`) ou o estado vazio com "Abrir a Conversa". O "Pedir correção ao Arquiteto" vai pela
 * Conversa (mesmo caminho do campo), que é aberta para o usuário acompanhar a resposta.
 */
import { memo, useCallback } from "react";
import { EmptyState } from "../../desktop/EmptyState";
import { useShell, useSpace } from "../../desktop/shellContext";
import { requestChatSend, useChatSending } from "../conversation/chatChannel";
import type { AppContentProps } from "../registry";
import { useT } from "../../i18n/react";
import { gateBlocked } from "../shared/readiness";
import "../shared/apps.css";
import { PlanCard } from "./PlanCard";
import { planOf } from "./summary";

function PlanContentView(_props: AppContentProps) {
  const space = useSpace();
  const shell = useShell();
  const { t } = useT();
  const projectId = space?.data.projectId ?? "";
  const sending = useChatSending(projectId);
  const openWindow = shell.openWindow;
  const onRequestFix = useCallback(
    (text: string) => {
      requestChatSend(projectId, text);
      openWindow("conversation");
    },
    [projectId, openWindow],
  );
  if (space === null) return null;
  const spec = planOf(space.data);
  if (spec === null) {
    return <EmptyState icon="plan" message={t("windowEmpty.plan")} action={{ label: t("windowEmpty.openConversation"), onClick: () => openWindow("conversation") }} />;
  }
  return (
    <div className="nd-app-scroll nd-app-page" data-plan-window="">
      <PlanCard
        projectId={projectId}
        spec={spec}
        stage={space.data.planStage}
        onApproved={space.data.reload}
        onRequestFix={onRequestFix}
        fixDisabled={sending || gateBlocked(shell.readiness, "chat")}
      />
    </div>
  );
}

export const PlanContent = memo(PlanContentView);
