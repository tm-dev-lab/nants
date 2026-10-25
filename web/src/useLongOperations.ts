import { useMemo } from "react";
import { longOperationsKey, parseLongOperations, type LongOperation } from "./longOperation";
import { useAppStore } from "./store";

/**
 * Operações longas em curso do projeto (spec 123 T02). Assina só a assinatura delas: a rajada de
 * atividade (raciocínio, texto, sinais de vida) não re-renderiza quem usa.
 */
export function useLongOperations(projectId: string): LongOperation[] {
  const key = useAppStore((s) => longOperationsKey(s.activities[projectId]));
  return useMemo(() => parseLongOperations(key), [key]);
}
