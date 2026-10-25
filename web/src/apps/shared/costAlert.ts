/*
 * Aviso de custo por tarefa (spec 124 T06). O servidor decide o patamar (3 vezes a mediana das
 * tarefas concluídas da spec ou o patamar fixo) e manda `cost_alerts` no estado; aqui só se monta o
 * texto. É um sinal discreto: a execução nunca para por ele.
 */
import type { CostAlertView, SpecView } from "../../api/types";
import { formatNumber, t, tp } from "../../i18n";
import { formatTokens } from "../../usageFormat";

export interface TaskCostAlert {
  task: string;
  alert: CostAlertView;
}

/** Texto completo do aviso: "T05: 16,2M tokens e 637 chamadas, 5 vezes a mediana...". */
export function costAlertText(task: string, alert: CostAlertView): string {
  const params = { task, tokens: formatTokens(alert.tokens), calls: tp("cost.alert.calls", alert.calls) };
  if (alert.basis === "relative" && typeof alert.ratio === "number" && Number.isFinite(alert.ratio)) {
    return t("cost.alert.relative", { ...params, ratio: formatNumber(alert.ratio) });
  }
  return t("cost.alert.absolute", params);
}

/** Avisos das tarefas ainda não concluídas da spec, na ordem das tarefas. */
export function openCostAlerts(spec: SpecView | null): TaskCostAlert[] {
  const alerts = spec?.cost_alerts;
  if (spec === null || alerts === undefined) return [];
  return spec.tasks.flatMap((task) => {
    const alert = alerts[task.id];
    return alert !== undefined && task.status !== "DONE" ? [{ task: task.id, alert }] : [];
  });
}
