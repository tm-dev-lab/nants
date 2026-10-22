/*
 * Visão de rede em uso (spec 097): a da última resposta do app (`GET .../app`, mais recente) ou, sem
 * ela, a de `GET /api/config`. `null` em servidores antigos.
 */
import type { NetworkView } from "../api/types";
import { useAppStore } from "../store";
import { parseNetworkView } from "./network";

export function useNetworkView(fromApp: NetworkView | null = null): NetworkView | null {
  const raw = useAppStore((s) => s.config?.network);
  if (fromApp !== null) return fromApp;
  return parseNetworkView(raw);
}
