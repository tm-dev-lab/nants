/*
 * Carregamento de `GET /api/config` (projetos e agentes) para o store.
 * Erro é guardado como a falha (`configError`, com `code` ou a reserva do cliente); nunca o corpo cru.
 */
import { getConfig } from "./api/client";
import { appStore, isProjectRemoved } from "./store";

export async function loadConfig(): Promise<void> {
  const result = await getConfig();
  if (result.ok) {
    // Carga iniciada antes da remoção pode trazer o projeto de volta: descarta os removidos.
    const removed = appStore.getState();
    const projects = result.data.projects.filter((p) => !isProjectRemoved(removed, p.id));
    appStore.setState({ config: { ...result.data, projects }, configError: null });
  } else {
    appStore.setState({ configError: result.error });
  }
}
