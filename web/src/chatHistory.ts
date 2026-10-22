/*
 * Carga do histórico do chat do Arquiteto (`GET /api/projects/:id/chat`) para o store.
 * - Chamada uma vez por abertura do projeto (a tela chama ao montar ou trocar de projeto).
 * - Só a resposta mais recente do mesmo projeto é aplicada (sequência por projeto).
 * - Falha de rede/HTTP é silenciosa: a conversa segue como está, sem alerta.
 */
import { getChatHistory } from "./api/client";
import { appStore, isProjectRemoved } from "./store";

/** Último número de sequência emitido por projeto. */
const latestSeq = new Map<string, number>();

/** Carrega o histórico e mescla no store. Nunca lança. */
export async function loadChatHistory(projectId: string): Promise<void> {
  const seq = (latestSeq.get(projectId) ?? 0) + 1;
  latestSeq.set(projectId, seq);
  const result = await getChatHistory(projectId);
  if (latestSeq.get(projectId) !== seq) return; // superada por uma carga mais nova
  if (isProjectRemoved(appStore.getState(), projectId)) return; // projeto removido nesta sessão
  if (!result.ok) return;
  appStore.hydrateHistory(projectId, result.data.turns);
}
