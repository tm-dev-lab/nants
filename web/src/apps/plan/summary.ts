/*
 * Plano da janela e o resumo dela (funções puras): subtítulo "<spec> · <situação>", selo de
 * plano pronto para aprovar e o estado vazio.
 */
import type { SpecView } from "../../api/types";
import type { SpaceData } from "../../desktop/useSpaceData";
import { localized, t } from "../../i18n";
import type { PlanStage } from "../../phase";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Estado vazio do Plano, no idioma ativo. */
function planEmptyMessage(): string {
  return t("windowEmpty.plan");
}

/** Situação da spec fora da aprovação (rótulo curto), no idioma ativo. */
export const SPEC_STATUS_LABELS: Readonly<Record<string, string>> = localized({
  DRAFT: "plan.status.DRAFT",
  AWAITING_APPROVAL: "plan.status.AWAITING_APPROVAL",
  IN_PROGRESS: "plan.status.IN_PROGRESS",
  COMPLETED: "plan.status.COMPLETED",
  BLOCKED: "plan.status.BLOCKED",
});

/** Spec exibida no Plano: a ativa, quando já tem tarefas ou aguarda aprovação. */
export function planOf(space: Pick<SpaceData, "activeSpec">): SpecView | null {
  const spec = space.activeSpec;
  if (spec === null) return null;
  if (spec.tasks.length === 0 && spec.status !== "AWAITING_APPROVAL") return null;
  return spec;
}

/** O plano pode ser aprovado agora: só na etapa "Aguardando sua aprovação" (`planStage`). */
export function planApprovable(space: Pick<SpaceData, "planStage">): boolean {
  return space.planStage === "awaiting";
}

/** Situação do plano em cada etapa (subtítulo da janela Plano). */
const STAGE_LABELS: Readonly<Record<PlanStage, string>> = localized({
  planning: "plan.stage.planning",
  reviewing: "plan.stage.reviewing",
  awaiting: "plan.stage.awaiting",
});

export function planSummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  const spec = planOf(space);
  if (spec === null) {
    return { subtitle: t("plan.noPlan"), empty: { message: planEmptyMessage() } };
  }
  const status = space.planStage !== null ? STAGE_LABELS[space.planStage] : (SPEC_STATUS_LABELS[spec.status ?? ""] ?? t("plan.status.fallback"));
  const subtitle = `${spec.id} · ${status}`;
  if (planApprovable(space)) return { subtitle, badge: { count: 1, label: t("plan.readyBadge") } };
  return { subtitle };
}
