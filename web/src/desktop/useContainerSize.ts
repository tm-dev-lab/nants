/*
 * Tamanho do contêiner de uma janela (container query em JS): observa a largura com
 * `ResizeObserver` (injetável; jsdom não tem layout) e define `data-size` no elemento,
 * para o conteúdo se adaptar à janela e não ao viewport.
 */
import { useEffect, useState, type RefObject } from "react";

export type ContainerSize = "narrow" | "medium" | "wide";

/** Limites de largura (px): abaixo de `narrow` é estreita; até `medium`, média; acima, ampla. */
const CONTAINER_BREAKPOINTS = { narrow: 480, medium: 840 } as const;

export function sizeForWidth(width: number): ContainerSize {
  if (width < CONTAINER_BREAKPOINTS.narrow) return "narrow";
  if (width < CONTAINER_BREAKPOINTS.medium) return "medium";
  return "wide";
}

/** Subconjunto de `ResizeObserver` usado aqui. */
export interface ResizeObserverLike {
  observe(target: Element): void;
  disconnect(): void;
}

export type ResizeObserverFactory = (
  callback: (entries: readonly { contentRect: { width: number } }[]) => void,
) => ResizeObserverLike;

const defaultFactory: ResizeObserverFactory | null =
  typeof ResizeObserver === "function" ? (callback) => new ResizeObserver((entries) => callback(entries)) : null;

interface ContainerSizeOptions {
  /** Fábrica do observador (testes); sem ela e sem `ResizeObserver`, mede só uma vez. */
  createObserver?: ResizeObserverFactory | null;
  /** Classe inicial, antes da primeira medição. */
  initial?: ContainerSize;
}

export function useContainerSize(ref: RefObject<HTMLElement | null>, options: ContainerSizeOptions = {}): ContainerSize {
  const [size, setSize] = useState<ContainerSize>(options.initial ?? "wide");
  const factory = options.createObserver === undefined ? defaultFactory : options.createObserver;

  useEffect(() => {
    const element = ref.current;
    if (element === null) return undefined;
    const apply = (width: number): void => {
      const next = sizeForWidth(width);
      element.dataset.size = next;
      setSize(next);
    };
    const measured = element.getBoundingClientRect().width;
    // jsdom mede 0: sem medição real, mantém a classe inicial até o observador falar.
    if (measured > 0) apply(measured);
    else element.dataset.size = options.initial ?? "wide";
    if (factory === null) return undefined;
    const observer = factory((entries) => {
      const last = entries[entries.length - 1];
      if (last !== undefined) apply(last.contentRect.width);
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref, factory, options.initial]);

  return size;
}
