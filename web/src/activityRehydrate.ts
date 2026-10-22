/*
 * Reidratação dos balões de atividade (spec 026): a atividade é efêmera, então ao abrir o projeto
 * e a cada reconexão do SSE busca o instantâneo do servidor e mescla no store.
 * Só a resposta mais recente por projeto é aplicada; projeto removido é ignorado; falha é silenciosa.
 */
import { getProjectActivity } from "./api/client";
import { appStore, isProjectRemoved } from "./store";

const latestSeq = new Map<string, number>();

export async function refreshActivities(projectId: string): Promise<void> {
  const seq = (latestSeq.get(projectId) ?? 0) + 1;
  latestSeq.set(projectId, seq);
  try {
    const result = await getProjectActivity(projectId);
    if (!result.ok) return;
    if (latestSeq.get(projectId) !== seq) return;
    if (isProjectRemoved(appStore.getState(), projectId)) return;
    appStore.mergeActivities(projectId, result.data, Date.now());
  } catch {
    // silencioso: a bolha reaparece no próximo evento
  }
}

/** Reconexão: reidrata o projeto aberto e os que já têm estado/atividade conhecidos. */
export async function refreshAllActivities(): Promise<void> {
  const s = appStore.getState();
  const ids = new Set<string>(Object.keys(s.projectStates));
  if (s.activeProjectId !== null && s.activeProjectId !== undefined) ids.add(s.activeProjectId);
  await Promise.all([...ids].map((id) => refreshActivities(id)));
}
