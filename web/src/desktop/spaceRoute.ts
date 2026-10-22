/*
 * URL do desktop (spec 093, decisão 1): não há rotas. O endereço reflete só o espaço ativo,
 * `/?space=<projectId>`. Qualquer outro caminho ou parâmetro é normalizado com `replaceState`,
 * sem conhecer nomes de rotas antigas. Trocar de espaço usa `pushState` (voltar funciona).
 */
import { useCallback, useEffect, useSyncExternalStore } from "react";

const SPACE_PARAM = "space";

/** Mesmo formato de id de projeto aceito pelo servidor. */
const PROJECT_ID = /^[A-Za-z0-9_-]{1,64}$/;

function isProjectId(value: string): boolean {
  return PROJECT_ID.test(value);
}

/** URL canônica do espaço (`/` sem espaço). */
export function spaceUrl(space: string | null): string {
  return space === null ? "/" : `/?${SPACE_PARAM}=${encodeURIComponent(space)}`;
}

/** Espaço pedido pela URL e a URL canônica correspondente. */
export function parseSpaceLocation(search: string): { space: string | null; canonical: string } {
  const value = new URLSearchParams(search).get(SPACE_PARAM);
  const space = value !== null && isProjectId(value) ? value : null;
  return { space, canonical: spaceUrl(space) };
}

/** URL atual precisa ser normalizada? */
export function needsNormalize(pathname: string, search: string, hash = ""): string | null {
  const { canonical } = parseSpaceLocation(search);
  return `${pathname}${search}${hash}` === canonical ? null : canonical;
}

const SPACE_EVENT = "nants:spacechange";

function subscribe(listener: () => void): () => void {
  window.addEventListener("popstate", listener);
  window.addEventListener(SPACE_EVENT, listener);
  return () => {
    window.removeEventListener("popstate", listener);
    window.removeEventListener(SPACE_EVENT, listener);
  };
}

function currentSpace(): string | null {
  return parseSpaceLocation(window.location.search).space;
}

/** Normaliza a URL atual (`replaceState`) quando ela não é a canônica. */
function normalizeLocation(): void {
  const target = needsNormalize(window.location.pathname, window.location.search, window.location.hash);
  if (target !== null) window.history.replaceState(window.history.state, "", target);
}

/** Vai para o espaço (`null` = boas-vindas). */
function goToSpace(space: string | null): void {
  const target = spaceUrl(space);
  if (`${window.location.pathname}${window.location.search}` !== target) window.history.pushState({}, "", target);
  window.dispatchEvent(new Event(SPACE_EVENT));
}

/**
 * Troca de espaço sem deixar o atual no histórico (`replaceState`): usado quando o espaço deixa
 * de existir (projeto removido), para o "voltar" não levar a um espaço inexistente.
 */
export function replaceSpace(space: string | null): void {
  const target = spaceUrl(space);
  if (`${window.location.pathname}${window.location.search}` !== target) window.history.replaceState({}, "", target);
  window.dispatchEvent(new Event(SPACE_EVENT));
}

/** Espaço ativo, reativo a `goToSpace` e a voltar/avançar. Normaliza a URL ao montar e a cada troca. */
export function useSpaceRoute(): [string | null, (space: string | null) => void] {
  const space = useSyncExternalStore(subscribe, currentSpace, () => null);
  useEffect(() => {
    normalizeLocation();
    const onChange = (): void => normalizeLocation();
    window.addEventListener("popstate", onChange);
    return () => window.removeEventListener("popstate", onChange);
  }, []);
  const go = useCallback((next: string | null) => goToSpace(next), []);
  return [space, go];
}
