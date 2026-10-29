/*
 * Armazenamento local do navegador, ou `null` se indisponível (sempre em try/catch). Único acesso
 * a ele fora da limpeza na carga: o cache do ui-state e o espelho do idioma (spec 101) passam por
 * aqui. Módulo sem dependências: o i18n o usa sem importar o ui-state nem o cliente da API.
 */
export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export function defaultStorage(): StorageLike | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
