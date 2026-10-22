/*
 * Uso do projeto (`GET /api/projects/:id/usage`, com ou sem `spec`) para o Quadro (tempo e custo
 * por tarefa) e os Custos: relido a cada
 * mudança do estado e na recarga ao vivo com debounce, sem polling. Só aceita o contrato
 * completo; falha de carga não mostra nada (o custo é informativo, nunca bloqueia).
 */
import { useEffect, useState } from "react";
import { getUsage } from "../../api/client";
import type { TaskUsage, UsageView } from "../../api/types";
import { useAppStore } from "../../store";

/** Resposta com o contrato completo (listas e totais numéricos). */
function isUsageView(value: UsageView | null | undefined): value is UsageView {
  return value !== null && value !== undefined && Array.isArray(value.specs) && typeof value.total?.input === "number";
}

/**
 * Uso do projeto (`specId` nulo) ou da spec. `enabled` falso não busca (janela fora de vista);
 * o último valor carregado continua valendo.
 */
export function useUsageView(projectId: string, specId: string | null, enabled = true): UsageView | null {
  const revision = useAppStore((s) => s.revisions[projectId] ?? 0);
  const liveRevision = useAppStore((s) => s.liveRevisions[projectId] ?? 0);
  const key = `${projectId}\u0000${specId ?? ""}`;
  const [state, setState] = useState<{ key: string; view: UsageView } | null>(null);

  useEffect(() => {
    if (!enabled) return undefined;
    let current = true;
    void (async () => {
      try {
        const r = specId === null ? await getUsage(projectId) : await getUsage(projectId, specId);
        if (current && r.ok && isUsageView(r.data)) setState({ key, view: r.data });
      } catch {
        // Informativo: falha de carga nunca quebra a janela.
      }
    })();
    return () => {
      current = false;
    };
  }, [projectId, specId, enabled, key, revision, liveRevision]);

  return state !== null && state.key === key ? state.view : null;
}

export type TaskUsageMap = Readonly<Record<string, TaskUsage>>;

const NO_TASKS: TaskUsageMap = {};

/** Uso por tarefa da spec (cartão do Quadro e barras por tarefa). */
export function taskUsageOf(view: UsageView | null, specId: string | null): TaskUsageMap {
  if (view === null || specId === null) return NO_TASKS;
  const tasks = view.specs.find((s) => s.spec === specId)?.tasks;
  if (!Array.isArray(tasks)) return NO_TASKS;
  const map: Record<string, TaskUsage> = {};
  for (const t of tasks) if (typeof t.id === "string") map[t.id] = t;
  return map;
}
