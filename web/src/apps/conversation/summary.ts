/*
 * Subtítulo vivo e selo do dock da Conversa (função pura, valores primitivos):
 * - subtítulo: o que falta para conversar, pedidos pendentes, a etapa do plano ("Planejando",
 *   "Revisando o plano", "Aguardando sua aprovação", pela fase derivada de `planStage`),
 *   "Arquiteto pensando" ou a fase;
 * - selo: pendências da Conversa (permissões e perguntas sem resposta) mais as mensagens novas
 *   que chegaram com a janela fora de vista ("N novas").
 */
import { t, tp } from "../../i18n";
import { PHASE_LABELS } from "../../phase";
import type { AppSummary, AppSummaryInput } from "../registry";

export function conversationSummary({ space, readiness, attention, conversationUnseen = 0 }: AppSummaryInput): AppSummary {
  if (space === null) return {};
  const permissions = space.permissions.length;
  const choices = attention?.pending_choices === true ? 1 : 0;
  const pending = permissions + choices;
  let subtitle: string;
  if (readiness !== null && !readiness.chat.ok) subtitle = t("conversation.summary.configure");
  else if (permissions > 0) subtitle = tp("conversation.summary.permissions", permissions);
  else if (choices > 0) subtitle = t("conversation.summary.questions");
  else if (space.architectTurnActive && space.planStage === null) subtitle = t("conversation.summary.thinking");
  else subtitle = t("conversation.summary.phase", { phase: PHASE_LABELS[space.phase].toLowerCase() });
  const count = pending + conversationUnseen;
  if (count === 0) return { subtitle };
  const parts: string[] = [];
  if (pending > 0) parts.push(tp("conversation.summary.pending", pending));
  if (conversationUnseen > 0) parts.push(tp("conversation.summary.newMessages", conversationUnseen));
  return { subtitle, badge: { count, label: parts.join(", ") } };
}
