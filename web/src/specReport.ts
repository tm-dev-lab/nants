/*
 * Carga do relatório do Arquiteto de uma spec (`GET .../specs/:spec_id/report`).
 * - Sequência anti-atraso por PROJETO: só a carga mais nova do projeto é aplicada.
 * - Identidade de spec: a resposta é descartada se a spec deixou de ser a ativa do projeto
 *   ou se o projeto foi removido enquanto a resposta chegava.
 * - 404 é "sem relatório" (fallback para o painel de fatos); demais falhas são `failed`.
 */
import { getSpecReport } from "./api/client";
import type { SpecReport } from "./api/types";
import { activeSpecIdOf } from "./dialogHistory";
import { appStore, isProjectRemoved } from "./store";

type ReportLoad =
  | { kind: "ready"; report: SpecReport }
  | { kind: "absent" }
  | { kind: "failed" }
  | { kind: "stale" };

/** Última carga de relatório emitida por projeto. */
const reportSeq = new Map<string, number>();

export async function loadSpecReport(projectId: string, specId: string): Promise<ReportLoad> {
  const seq = (reportSeq.get(projectId) ?? 0) + 1;
  reportSeq.set(projectId, seq);
  const result = await getSpecReport(projectId, specId);
  const superseded =
    reportSeq.get(projectId) !== seq ||
    isProjectRemoved(appStore.getState(), projectId) ||
    activeSpecIdOf(projectId) !== specId;
  if (superseded) return { kind: "stale" };
  if (result.ok) return { kind: "ready", report: result.data };
  if (result.error.kind === "http" && result.error.status === 404) return { kind: "absent" };
  return { kind: "failed" };
}
