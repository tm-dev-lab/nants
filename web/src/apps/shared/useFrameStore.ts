/*
 * Leitura do store limitada por quadro (spec 093, decisão 4, desempenho): a atividade ao vivo
 * chega em rajadas (vários eventos por quadro); a janela só re-renderiza uma vez por quadro
 * (rAF), com o valor mais recente. O agendador vem do Desktop (`WindowHost.scheduler`, que os
 * testes trocam pelo manual); sem ele, `requestAnimationFrame` (ou um temporizador de 16 ms).
 */
import { useEffect, useRef, useState } from "react";
import { useWindowHost } from "../../desktop/windowHost";
import type { FrameScheduler } from "../../desktop/useWindowGesture";
import { appStore, type AppState } from "../../store";

const FRAME_MS = 16;

/** Agendador padrão: rAF quando existe; senão, um temporizador do tamanho de um quadro. */
const DEFAULT_FRAME_SCHEDULER: FrameScheduler =
  typeof requestAnimationFrame === "function" && typeof cancelAnimationFrame === "function"
    ? { request: (callback) => requestAnimationFrame(() => callback()), cancel: (id) => cancelAnimationFrame(id) }
    : {
        request: (callback) => setTimeout(callback, FRAME_MS) as unknown as number,
        cancel: (id) => clearTimeout(id),
      };

/**
 * Valor do store pelo `select`, publicado no máximo uma vez por quadro. `key` identifica a fonte
 * (o projeto): ao mudar, o valor é relido na hora. O `select` precisa devolver uma referência
 * estável enquanto nada mudou (como os seletores do store).
 */
export function useStoreByFrame<T>(key: string, select: (state: AppState) => T): T {
  const host = useWindowHost();
  const scheduler = host.scheduler ?? DEFAULT_FRAME_SCHEDULER;
  const selectRef = useRef(select);
  selectRef.current = select;
  const [shown, setShown] = useState(() => ({ key, value: select(appStore.getState()) }));
  let current = shown;
  if (shown.key !== key) {
    current = { key, value: select(appStore.getState()) };
    setShown(current);
  }
  const shownRef = useRef(current);
  shownRef.current = current;

  useEffect(() => {
    let pending: number | null = null;
    const flush = (): void => {
      pending = null;
      const next = selectRef.current(appStore.getState());
      if (Object.is(next, shownRef.current.value)) return;
      const value = { key, value: next };
      shownRef.current = value;
      setShown(value);
    };
    const unsubscribe = appStore.subscribe(() => {
      if (pending !== null) return;
      if (Object.is(selectRef.current(appStore.getState()), shownRef.current.value)) return;
      pending = scheduler.request(flush);
    });
    // Algo pode ter mudado entre o render e a assinatura.
    if (!Object.is(selectRef.current(appStore.getState()), shownRef.current.value)) pending = scheduler.request(flush);
    return () => {
      unsubscribe();
      if (pending !== null) scheduler.cancel(pending);
    };
  }, [key, scheduler]);

  return current.value;
}
