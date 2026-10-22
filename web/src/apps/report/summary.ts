/*
 * Resumo da janela Relatório (função pura): subtítulo pela situação do relatório e o estado
 * vazio antes da conclusão. Sem selo: o relatório pronto não exige ação.
 */
import type { ResultTask } from "../../api/types";
import { t } from "../../i18n";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Tarefas integradas (DONE) do resultado. */
export function integratedCount(tasks: readonly Pick<ResultTask, "status">[]): number {
  return tasks.filter((task) => task.status === "DONE").length;
}

export function reportSummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  const spec = space.activeSpec;
  if (spec === null || spec.status !== "COMPLETED") return { subtitle: t("summary.waitingCompletion"), empty: { message: t("windowEmpty.report") } };
  if (space.state?.report_pending === true) return { subtitle: t("report.subtitle.writing") };
  return { subtitle: t("report.subtitle.done", { spec: spec.id }) };
}
