/*
 * Indicador "Usando o plano ChatGPT" (spec 116 T03), perto do campo da Conversa, quando algum papel
 * usa uma conexão pelo plano. Traz o link "Gerenciar uso" (a página de uso da conta ChatGPT).
 */
import { useT } from "../../i18n/react";
import { useAppStore } from "../../store";
import { agentsUsePlan, PLAN_USAGE_URL } from "../settings/chatgptPlan";

export function PlanIndicator() {
  const { t } = useT();
  const usesPlan = useAppStore((s) => agentsUsePlan(s.config?.agents, s.config?.connections));
  if (!usesPlan) return null;
  return (
    <p className="nd-plan-indicator" data-plan-indicator="">
      <span className="nd-dot" data-tone="violet" aria-hidden="true" />
      <span>{t("chatgpt.indicator")}</span>
      <a className="nd-link" href={PLAN_USAGE_URL} target="_blank" rel="noopener noreferrer" data-plan-usage="">
        {t("chatgpt.manageUsage")}
      </a>
    </p>
  );
}
