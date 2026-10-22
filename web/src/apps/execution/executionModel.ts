/*
 * Regras puras da janela Execução (spec 095, T03): o subtítulo vivo. O estado da execução em si
 * vem da sessão compartilhada com o App (`apps/app/runSession.ts`).
 */
import { t } from "../../i18n";
import type { AppSummary, AppSummaryInput } from "../registry";

/** Subtítulo: a especificação concluída (a que "Iniciar" sobe) ou a espera. */
export function executionSummary({ space }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  const spec = space.activeSpec;
  if (spec === null || spec.status !== "COMPLETED") return { subtitle: t("summary.waitingCompletion") };
  return { subtitle: `${spec.id} · docker compose` };
}
