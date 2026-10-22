/*
 * Limpeza do armazenamento do navegador na carga. O estado da interface fica no servidor
 * (ui-state, com o cache `nants.ui-state.*` no `localStorage`); as chaves abaixo não pertencem a
 * esse formato e são apagadas uma vez por carga, sem tocar em nada fora desta lista.
 * Lista mínima e explícita: prefixos (o sufixo é a chave antiga ou o id do projeto).
 *
 * Nome antigo do produto (spec 100, decisão 1): o usuário zera a base, mas não o navegador. Tudo
 * sob `LEGACY_PREFIX` (inclusive o cache e os rascunhos do ui-state antigo) é lixo e sai.
 * Este é o único lugar da interface que ainda cita o nome antigo.
 */
export const LEGACY_PREFIX = "nemo.";

/** `localStorage`: chaves exatas. */
export const STALE_LOCAL_KEYS: readonly string[] = [];

/** `localStorage`: prefixos. */
export const STALE_LOCAL_PREFIXES: readonly string[] = [LEGACY_PREFIX];

/** `sessionStorage`: prefixos. */
export const STALE_SESSION_PREFIXES: readonly string[] = [LEGACY_PREFIX];

function clear(storage: Storage, exact: readonly string[], prefixes: readonly string[]): number {
  const doomed: string[] = [];
  for (let i = 0; i < storage.length; i += 1) {
    const key = storage.key(i);
    if (key === null) continue;
    if (exact.includes(key) || prefixes.some((prefix) => key.startsWith(prefix))) doomed.push(key);
  }
  for (const key of doomed) storage.removeItem(key);
  return doomed.length;
}

function storageOf(get: () => Storage): Storage | null {
  try {
    return get();
  } catch {
    // Armazenamento bloqueado (modo privado, política do navegador): nada a limpar.
    return null;
  }
}

/** Apaga as chaves da lista. Devolve quantas saíram. Nunca lança. */
export function clearStaleStorage(
  local: Storage | null = storageOf(() => window.localStorage),
  session: Storage | null = storageOf(() => window.sessionStorage),
): number {
  let removed = 0;
  try {
    if (local !== null) removed += clear(local, STALE_LOCAL_KEYS, STALE_LOCAL_PREFIXES);
    if (session !== null) removed += clear(session, [], STALE_SESSION_PREFIXES);
  } catch {
    // Falha ao ler ou apagar: a interface segue sem a limpeza.
  }
  return removed;
}
