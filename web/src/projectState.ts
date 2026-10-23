/*
 * Carregamento do estado de um projeto para o store. Único ponto que grava `projectStates`
 * a partir de `GET /api/projects/:id/state`, e que reidrata a fila de permissões a partir de
 * `pending_permissions`.
 * - Sequência por projeto: só a resposta do GET mais recente é aplicada (respostas fora de
 *   ordem são descartadas e devolvem `null`).
 * - Pedidos que chegaram pelo stream depois do início do GET não são removidos pela hidratação.
 * - Turno do Arquiteto que a tela mostrava e o servidor não tem mais: some e o histórico é recarregado.
 */
import { getProjectState } from "./api/client";
import type { ApiResult } from "./api/client";
import type { ProjectState } from "./api/types";
import { loadChatHistory } from "./chatHistory";
import { appStore, arrivalClockNow, turnStartClockNow, dropAnalyzing, isProjectRemoved } from "./store";

/** Último número de sequência emitido por projeto. */
const latestSeq = new Map<string, number>();

/**
 * Busca o estado e aplica no store. Devolve `null` quando a resposta foi superada por um GET
 * mais novo do mesmo projeto (nada é aplicado nesse caso).
 */
export async function refreshProjectState(projectId: string): Promise<ApiResult<ProjectState> | null> {
  const seq = (latestSeq.get(projectId) ?? 0) + 1;
  latestSeq.set(projectId, seq);
  const startedAt = arrivalClockNow();
  const turnClock = turnStartClockNow();

  const result = await getProjectState(projectId);
  // Projeto removido nesta sessão: a resposta atrasada não recria estado.
  if (isProjectRemoved(appStore.getState(), projectId)) return null;
  const superseded = latestSeq.get(projectId) !== seq;

  if (result.ok) {
    const data = result.data;
    if (!superseded) {
      // Spec 114 T04: a transição para "executando" marca o início ou a retomada da execução.
      const started = data.run !== null && (appStore.getState().projectStates[projectId]?.run ?? null) === null;
      appStore.setState((s) => ({
        projectStates: { ...s.projectStates, [projectId]: data },
        // Sem run ativo, nenhuma análise está em andamento.
        ...(!data.running && data.run === null ? { recoveries: dropAnalyzing(s.recoveries, projectId) } : {}),
      }));
      if (started) appStore.markRunStarted(projectId);
      appStore.hydratePermissions(projectId, data.pending_permissions ?? [], startedAt);
      const hadTurn = appStore.getState().architectTurns[projectId] !== undefined;
      appStore.syncArchitectTurn(projectId, data.architect_turn ?? null, turnClock);
      // O servidor diz que o turno acabou e a tela ainda o mostrava (evento de fim perdido, aba
      // congelada, POST derrubado): o "pensando" já saiu; a resposta vem do histórico persistido.
      if (hadTurn && appStore.getState().architectTurns[projectId] === undefined) void loadChatHistory(projectId);
    } else if (appStore.getState().projectStates[projectId] === undefined) {
      // GET superado sem nenhum estado aplicado: é melhor que nada. Nunca sobrescreve um
      // estado mais novo (esta condição só vale quando não há estado algum).
      appStore.setState((s) => ({ projectStates: { ...s.projectStates, [projectId]: data } }));
    }
  }
  return superseded ? null : result;
}
