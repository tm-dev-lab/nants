/*
 * Trilha de ações (`tools.jsonl`, spec 085) para as janelas Atividade (últimas ações da tarefa
 * ativa, imagens geradas no histórico): a página mais recente, relida a cada mudança do estado
 * (`revisions`) e na recarga ao vivo com debounce (`liveRevisions`), sem polling. Com a janela
 * fora de vista (`enabled` falso), não busca; ao voltar, relê. Falha de carga mantém o que já
 * estava na tela (informativo).
 */
import { useEffect, useState } from "react";
import { getToolActions } from "../../api/client";
import type { ToolActionItem } from "../../api/types";
import { useAppStore } from "../../store";

const NO_ITEMS: readonly ToolActionItem[] = [];

interface ToolTrailQuery {
  projectId: string;
  specId: string | null;
  /** Filtro por tarefa (`FINAL` = verificação final); ausente = a spec toda. */
  task?: string | null;
  limit: number;
  enabled: boolean;
}

export function useToolTrail({ projectId, specId, task = null, limit, enabled }: ToolTrailQuery): readonly ToolActionItem[] {
  const revision = useAppStore((s) => s.revisions[projectId] ?? 0);
  const liveRevision = useAppStore((s) => s.liveRevisions[projectId] ?? 0);
  const key = `${projectId}\u0000${specId ?? ""}\u0000${task ?? ""}`;
  const [state, setState] = useState<{ key: string; items: readonly ToolActionItem[] }>({ key: "", items: NO_ITEMS });

  useEffect(() => {
    if (!enabled || specId === null) return undefined;
    let current = true;
    void getToolActions(projectId, specId, { ...(task !== null ? { task } : {}), limit })
      .then((r) => {
        if (current && r.ok) setState({ key, items: r.data.items });
      })
      .catch(() => undefined);
    return () => {
      current = false;
    };
  }, [projectId, specId, task, limit, enabled, key, revision, liveRevision]);

  return state.key === key ? state.items : NO_ITEMS;
}
