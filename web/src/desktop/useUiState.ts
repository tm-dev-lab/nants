/*
 * Hook do estado da UI de um escopo (global ou projeto). Cria um `UiStateSync` por escopo,
 * lê o servidor ao montar, manda as seções pendentes no `pagehide` (keepalive) e ao trocar de
 * escopo/desmontar, e expõe o snapshot por `useSyncExternalStore`.
 */
import { useCallback, useEffect, useMemo, useSyncExternalStore } from "react";
import { getSection, scopeKey, UiStateSync, type UiScope, type UiStateDeps, type UiStateSnapshot } from "./uiState";

export interface UiStateHandle {
  snapshot: UiStateSnapshot;
  get: (section: string) => unknown;
  /** Grava a seção (debounce ≤ 1/s). `false` se a seção não é permitida neste escopo. */
  set: (section: string, value: unknown) => boolean;
  flush: () => Promise<void>;
  sync: UiStateSync;
}

/** `deps` é lido só na criação do cliente de cada escopo. */
export function useUiState(scope: UiScope, deps?: UiStateDeps): UiStateHandle {
  const key = scopeKey(scope);
  // Um cliente por escopo: `scope` e `deps` mudam de identidade a cada render.
  const sync = useMemo(() => new UiStateSync(scope, deps), [key]);

  useEffect(() => {
    void sync.start();
    const onPageHide = (): void => sync.flushOnPageHide();
    // Aba oculta: a página ainda vive, então o envio normal (sequencial, com 409) funciona; no
    // celular o `pagehide` muitas vezes nem dispara.
    const onVisibility = (): void => {
      if (document.visibilityState === "hidden" && sync.hasPending()) void sync.flush();
    };
    window.addEventListener("pagehide", onPageHide);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.removeEventListener("pagehide", onPageHide);
      document.removeEventListener("visibilitychange", onVisibility);
      // Trocar de espaço ou desmontar: o que estava pendente ainda vai para o servidor.
      if (sync.hasPending()) void sync.flush();
      sync.stop();
    };
  }, [sync]);

  const snapshot = useSyncExternalStore(sync.subscribe, sync.getSnapshot, sync.getSnapshot);
  const get = useCallback((section: string) => sync.get(section), [sync]);
  const set = useCallback((section: string, value: unknown) => sync.set(section, value), [sync]);
  const flush = useCallback(() => sync.flush(), [sync]);
  // Identidade estável: só muda com o snapshot (ou o escopo).
  return useMemo(() => ({ snapshot, get, set, flush, sync }), [snapshot, get, set, flush, sync]);
}

/** Uma seção validada: `parse` recebe o valor cru (ou `undefined`) e devolve o padrão se inválido. */
export function useUiSection<T>(handle: UiStateHandle, section: string, parse: (value: unknown) => T): T {
  // `setSection` e a adoção de envelopes preservam a identidade das seções que não mudaram:
  // o valor parseado só é recriado quando a própria seção muda. `parse` deve ser estável.
  const rawValue = getSection(handle.snapshot.data, section);
  return useMemo(() => parse(rawValue), [rawValue, parse]);
}
